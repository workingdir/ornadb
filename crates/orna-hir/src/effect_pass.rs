//! The effect pass (design section 2.5, stage H3a).
//!
//! Every function's effect set is the union of the effects its own bodies
//! perform directly and the effects of every function those bodies call. The
//! call graph is condensed into strongly connected components (Tarjan), and
//! each component shares one set: its members reach one another, so they can
//! perform exactly the same effects. Components are emitted callee-first, so a
//! component's callees are final when it is computed.
//!
//! Direct effects follow the old checker's vocabulary (`orna_check`'s
//! `effects::host_effect` and its table-operation kinds):
//!
//! - a native or `sys` callee contributes the kind its declaration path maps
//!   to, plus `MayFail` whenever it has one;
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
            let ExprKind::Call(call) = &expr.kind else {
                continue;
            };
            match &call.callee {
                Callee::Source { def, .. } => calls[owner].push(*def),
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

/// The table kind a table operation touches.
fn table_kind(op: TableOp) -> EffectKind {
    match op {
        TableOp::Insert | TableOp::Upsert | TableOp::Update | TableOp::Delete | TableOp::Rekey => {
            EffectKind::TableWrite
        }
        TableOp::Count | TableOp::First | TableOp::One | TableOp::AsOf => EffectKind::TableRead,
    }
}

/// The effects of a native declaration, by the same path classification the
/// old checker applies. A declaration with no host kind contributes nothing.
fn native_effects(program: &Program, def: DefId) -> EffectSet {
    let definition = &program.defs[def];
    let module = &program.modules[definition.module];
    let mut path: Vec<&str> = module
        .path
        .segments()
        .iter()
        .map(crate::Name::as_str)
        .collect();
    path.push(definition.name.as_str());
    match host_effect(&path) {
        Some(kind) => EffectSet::single(kind).join(EffectSet::failing()),
        None => EffectSet::empty(),
    }
}

/// The host effect kind of a native declaration by its full path, as
/// `orna_check::effects::host_effect` classifies it.
fn host_effect(path: &[&str]) -> Option<EffectKind> {
    use EffectKind::{
        Clock, Filesystem, Git, Network, Process, Randomness, RuntimeAdmin, SecretAccess,
        StorageAdmin, Ui,
    };
    Some(match path {
        ["std", "net", ..] => Network,
        ["std", "io", "fs", ..] => Filesystem,
        ["std", "io", "process" | "environment", ..] => Process,
        ["std", "concurrent", "sleep"] => Clock,
        ["std", "random", ..] => Randomness,
        ["std", "secret", "open"] => SecretAccess,
        ["sys", "io", ..] => Filesystem,
        ["sys", "net", ..] => Network,
        ["sys", "process", ..] => Process,
        ["sys", "ui", ..] => Ui,
        ["sys", "clock", ..] => Clock,
        ["sys", "random", ..] => Randomness,
        ["sys", "blob", "capture_file"] => Filesystem,
        [
            "sys",
            "admin",
            "commit" | "commit_reviewed" | "checkout" | "create_branch",
        ] => Git,
        ["sys", "admin", "flush" | "compact" | "verify"] => StorageAdmin,
        [
            "sys",
            "admin",
            "cancel_run" | "pause_stream" | "resume_stream" | "reset_checkpoint" | "retry_failure"
            | "skip_failure" | "replay_failure" | "resolve_failure",
        ] => RuntimeAdmin,
        _ => return None,
    })
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
