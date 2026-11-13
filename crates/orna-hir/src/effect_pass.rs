//! The effect pass (design section 2.5, stage H3a).
//!
//! Every function's effect set is the union of the effects its own bodies
//! perform directly and the effects of every function those bodies call. The
//! call graph is condensed into strongly connected components (Tarjan), and
//! each component shares one set: its members reach one another, so they can
//! perform exactly the same effects. Components are emitted callee-first, so a
//! component's callees are final when it is computed.
//!
//! Direct effects follow the `effect` clause each native declaration carries
//! (ORNA-NATIVE-009):
//!
//! - a native or `sys` callee contributes the kinds its declared clause names,
//!   plus `MayFail` whenever the clause names any kind;
//! - a table operation contributes its table kind plus `MayFail`;
//! - a callee the classification cannot resolve (a member call or a first-class
//!   value) contributes `Opaque`, which fails closed.
//!
//! Lambda bodies belong to the definition whose body contains them, so their
//! effects are joined into that definition: a lambda's effect is reachable
//! through the expression that creates it.

use crate::{
    Callee, DefId, DefKind, EffectKind, EffectSet, ExprKind, FnBody, Idx, Program, RelOp, TableOp,
};

/// Computes `FnDef.effects` for every source function and `Body.effects` for
/// every body in `program`.
pub(crate) fn run(program: &mut Program) {
    let count = program.defs.len();
    let mut direct = vec![EffectSet::empty(); count];
    let mut calls: Vec<Vec<DefId>> = vec![Vec::new(); count];
    for body in program.bodies.iter() {
        let owner = body.owner.index();
        for expr in body.exprs.iter() {
            if let ExprKind::Def(def) = &expr.kind
                && (matches!(program.defs[*def].kind, DefKind::Table(_))
                    || (matches!(program.defs[*def].kind, DefKind::Type(_))
                        && program.is_sys_relation(*def)))
            {
                direct[owner] = direct[owner].join(table_scan());
                continue;
            }
            let ExprKind::Call(call) = &expr.kind else {
                continue;
            };
            match &call.callee {
                Callee::Source { def, .. } => calls[owner].push(*def),
                Callee::Value { callee } => {
                    match value_target(program, &body.exprs[*callee].kind) {
                        ValueTarget::Source(def) => calls[owner].push(def),
                        ValueTarget::Effects(effects) => {
                            direct[owner] = direct[owner].join(effects)
                        }
                    }
                }
                callee => direct[owner] = direct[owner].join(direct_effects(program, callee)),
            }
        }
    }
    let effects = component_effects(&direct, &calls);
    for id in program.defs.indices().collect::<Vec<_>>() {
        if let DefKind::Fn(function) = &mut program.defs[id].kind
            && matches!(function.body, FnBody::Source(_))
        {
            function.effects = effects[id.index()];
        }
    }
    for id in program.bodies.indices().collect::<Vec<_>>() {
        let owner = program.bodies[id].owner.index();
        program.bodies[id].effects = effects[owner];
    }
}

/// The effect set of every definition: its direct effects joined with those of
/// everything reachable through its source calls, shared per component.
fn component_effects(direct: &[EffectSet], calls: &[Vec<DefId>]) -> Vec<EffectSet> {
    let mut effects = vec![EffectSet::empty(); direct.len()];
    for component in strongly_connected(calls) {
        let mut shared = EffectSet::empty();
        for &member in &component {
            shared = shared.join(direct[member]);
            for callee in &calls[member] {
                shared = shared.join(effects[callee.index()]);
            }
        }
        for &member in &component {
            effects[member] = shared;
        }
    }
    effects
}

/// The effects one non-source callee performs by itself.
pub(crate) fn direct_effects(program: &Program, callee: &Callee) -> EffectSet {
    match callee {
        Callee::Native { def, .. } | Callee::Sys { def, .. } => native_effects(program, *def),
        Callee::TableOp { op, .. } => EffectSet::single(table_kind(*op)).join(EffectSet::failing()),
        Callee::Pending { .. } | Callee::Value { .. } => EffectSet::opaque(),
        Callee::Relational {
            op: RelOp::Count | RelOp::First,
        } => EffectSet::single(EffectKind::TableRead).join(EffectSet::failing()),
        Callee::Source { .. }
        | Callee::Relational { .. }
        | Callee::Intrinsic(_)
        | Callee::Ctor { .. } => EffectSet::empty(),
    }
}

/// What reading a table by name performs: a bare table reference is a scan of that table, which may fail (the
/// checker's qualified table path, `inference.rs`). A relation stage or pipe over the table takes it as its receiver.
fn table_scan() -> EffectSet {
    EffectSet::single(EffectKind::TableRead).join(EffectSet::failing())
}

/// What a call through a first-class value contributes: a named source function
/// is joined through the call graph, anything else is settled here.
enum ValueTarget {
    Source(DefId),
    Effects(EffectSet),
}

/// Resolves a value callee statically when its expression names the callee:
/// a `Def` function or a lambda. A lambda's body belongs to the definition that
/// contains it, so its effects are already in that definition's set. Any other
/// value (a local, a parameter, a member) stays opaque and fails closed.
fn value_target(program: &Program, kind: &ExprKind) -> ValueTarget {
    match kind {
        ExprKind::Def(def) => match &program.defs[*def].kind {
            DefKind::Fn(function) => match function.body {
                FnBody::Source(_) => ValueTarget::Source(*def),
                FnBody::Native(_) => ValueTarget::Effects(native_effects(program, *def)),
                FnBody::Signature => ValueTarget::Effects(EffectSet::opaque()),
            },
            _ => ValueTarget::Effects(EffectSet::opaque()),
        },
        ExprKind::Lambda { .. } => ValueTarget::Effects(EffectSet::empty()),
        _ => ValueTarget::Effects(EffectSet::opaque()),
    }
}

/// The table kind a table operation touches.
fn table_kind(op: TableOp) -> EffectKind {
    match op {
        TableOp::Insert | TableOp::Upsert | TableOp::Update | TableOp::Delete | TableOp::Rekey => {
            EffectKind::TableWrite
        }
        TableOp::Count | TableOp::First | TableOp::One | TableOp::AsOf => EffectKind::TableRead,
    }
}

/// The effects of a native declaration: the kinds its `effect` clause declares
/// (ORNA-NATIVE-009), plus `MayFail` whenever it has one. The clause is the
/// sole source; no path table is consulted, so a new native needs no pass edit.
/// `effect none` and a non-native callee contribute nothing.
fn native_effects(program: &Program, def: DefId) -> EffectSet {
    match &program.defs[def].kind {
        DefKind::Fn(function) => match &function.body {
            FnBody::Native(native) if !native.effects.is_empty() => {
                native.effects.join(EffectSet::failing())
            }
            _ => EffectSet::empty(),
        },
        _ => EffectSet::empty(),
    }
}

/// Strongly connected components of the call graph, callee-first (Tarjan).
fn strongly_connected(calls: &[Vec<DefId>]) -> Vec<Vec<usize>> {
    let mut state = Tarjan {
        calls,
        next: 0,
        index: vec![None; calls.len()],
        low: vec![0; calls.len()],
        on_stack: vec![false; calls.len()],
        stack: Vec::new(),
        components: Vec::new(),
    };
    for node in 0..calls.len() {
        if state.index[node].is_none() {
            state.visit(node);
        }
    }
    state.components
}

struct Tarjan<'a> {
    calls: &'a [Vec<DefId>],
    next: usize,
    index: Vec<Option<usize>>,
    low: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    components: Vec<Vec<usize>>,
}

impl Tarjan<'_> {
    fn visit(&mut self, node: usize) {
        self.index[node] = Some(self.next);
        self.low[node] = self.next;
        self.next += 1;
        self.stack.push(node);
        self.on_stack[node] = true;
        for callee in &self.calls[node] {
            let callee = callee.index();
            match self.index[callee] {
                None => {
                    self.visit(callee);
                    self.low[node] = self.low[node].min(self.low[callee]);
                }
                Some(callee_index) if self.on_stack[callee] => {
                    self.low[node] = self.low[node].min(callee_index);
                }
                Some(_) => {}
            }
        }
        if Some(self.low[node]) == self.index[node] {
            let mut component = Vec::new();
            while let Some(member) = self.stack.pop() {
                self.on_stack[member] = false;
                component.push(member);
                if member == node {
                    break;
                }
            }
            self.components.push(component);
        }
    }
}
