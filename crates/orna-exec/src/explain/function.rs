//! A function's explanation: its signature, its declared effects, and the tables its bodies reach.
//!
//! The walk follows the calls a function body makes to other Orna functions, and the table operations and table
//! names in each body, including lambda bodies. It evaluates nothing.

use orna_hir::{
    BodyId, Callee, DefId, DefKind, EffectSet, ExprKind, FnBody, FnDef, Program, print_signature,
};

/// What the explain of one function reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionExplain {
    /// The signature, as `name(params) -> result`.
    pub signature: String,
    /// The effects the function can perform, as the effect pass computed them.
    pub effects: EffectSet,
    /// The tables reached through table operations or table names, in first-reached order.
    pub tables: Vec<DefId>,
}

/// The explain of the function `def`, or `None` when `def` is not a function.
#[must_use]
pub fn explain_function(program: &Program, def: DefId) -> Option<FunctionExplain> {
    let signature = print_signature(program, def)?;
    let DefKind::Fn(function) = &program.defs[def].kind else {
        return None;
    };
    let mut walk = Walk::default();
    walk.function(program, def);
    Some(FunctionExplain {
        signature,
        effects: function.effects,
        tables: walk.tables,
    })
}

/// The definitions and bodies already visited, and the tables found so far.
#[derive(Default)]
struct Walk {
    functions: Vec<DefId>,
    bodies: Vec<BodyId>,
    tables: Vec<DefId>,
}

impl Walk {
    fn function(&mut self, program: &Program, def: DefId) {
        if self.functions.contains(&def) {
            return;
        }
        self.functions.push(def);
        if let DefKind::Fn(FnDef {
            body: FnBody::Source(body),
            ..
        }) = &program.defs[def].kind
        {
            self.body(program, *body);
        }
    }

    fn body(&mut self, program: &Program, id: BodyId) {
        if self.bodies.contains(&id) {
            return;
        }
        self.bodies.push(id);
        for expr in program.bodies[id].exprs.iter() {
            match &expr.kind {
                ExprKind::Def(def) if matches!(program.defs[*def].kind, DefKind::Table(_)) => {
                    self.table(*def)
                }
                ExprKind::Lambda { body, .. } => self.body(program, *body),
                ExprKind::Call(call) => match &call.callee {
                    Callee::TableOp { table, .. } => self.table(*table),
                    Callee::Source { def, .. } => self.function(program, *def),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn table(&mut self, def: DefId) {
        if !self.tables.contains(&def) {
            self.tables.push(def);
        }
    }
}
