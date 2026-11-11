//! The operator tree of a relation pipeline, read from the HIR the executor runs.
//!
//! The executor builds each pipeline from the call's HIR (see `relation::pipeline`): the source is the innermost input,
//! and each stage is applied over the one before it. This walk follows the same expressions, so every operator it
//! reports is one the executor applies. It evaluates nothing and reads no rows.

use orna_hir::{
    ArgValue, Body, BodyId, Call, Callee, DefId, DefKind, EffectKind, EffectSet, ExprId, ExprKind,
    Program, RelOp,
};

use crate::relation::is_stage;

/// The kind of one operator in a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatorKind {
    /// Every row of a table, in ascending key order.
    Scan,
    /// `filter`.
    Filter,
    /// `map`.
    Map,
    /// `take`.
    Take,
    /// `drop`.
    Drop,
    /// A source that is not a table: the elements of a list the expression evaluates to.
    External,
}

impl OperatorKind {
    /// The `sys.PlanNodeKind` the operator is reported as, if the sys type has a variant for it.
    #[must_use]
    pub const fn plan_node_kind(self) -> Option<&'static str> {
        match self {
            Self::Scan => Some("scan"),
            Self::Filter => Some("filter"),
            Self::Map => Some("project"),
            Self::Take => Some("limit"),
            Self::External => Some("external"),
            Self::Drop => None,
        }
    }
}

/// The function a stage calls once per row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callback {
    /// A named function.
    Def(DefId),
    /// A lambda written at the call site.
    Lambda(BodyId),
    /// A function value the walk cannot name, so its effects are unknown.
    Dynamic,
}

/// One operator of a pipeline, with the operator that feeds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operator {
    /// What the operator does.
    pub kind: OperatorKind,
    /// The table a scan reads.
    pub table: Option<DefId>,
    /// The function a `filter` or `map` calls per row.
    pub callback: Option<Callback>,
    /// The effects the operator itself performs: a scan reads a table, a callback performs its own effects.
    pub effects: EffectSet,
    /// The operator feeding this one; empty at a source.
    pub inputs: Vec<Operator>,
}

/// The operator tree of the relation expression `expr` in `body`, its root first.
#[must_use]
pub fn explain_query(program: &Program, body: &Body, expr: ExprId) -> Operator {
    operator(program, body, expr)
}

fn operator(program: &Program, body: &Body, expr: ExprId) -> Operator {
    match &body.exprs[expr].kind {
        ExprKind::Call(call) => match &call.callee {
            Callee::Relational { op } if is_stage(*op) => stage(program, body, call, *op),
            _ => external(),
        },
        ExprKind::Def(def) if matches!(program.defs[*def].kind, DefKind::Table(_)) => Operator {
            kind: OperatorKind::Scan,
            table: Some(*def),
            callback: None,
            effects: EffectSet::single(EffectKind::TableRead),
            inputs: Vec::new(),
        },
        _ => external(),
    }
}

fn stage(program: &Program, body: &Body, call: &Call, op: RelOp) -> Operator {
    let (kind, callback, effects) = match op {
        RelOp::Filter => {
            let (callback, effects) = callback(program, body, call);
            (OperatorKind::Filter, Some(callback), effects)
        }
        RelOp::Map => {
            let (callback, effects) = callback(program, body, call);
            (OperatorKind::Map, Some(callback), effects)
        }
        RelOp::Take => (OperatorKind::Take, None, EffectSet::empty()),
        RelOp::Drop => (OperatorKind::Drop, None, EffectSet::empty()),
        _ => return external(),
    };
    let inputs = match argument_expr(call, 0) {
        Some(input) => vec![operator(program, body, input)],
        None => Vec::new(),
    };
    Operator {
        kind,
        table: None,
        callback,
        effects,
        inputs,
    }
}

/// The callback a stage calls: argument 1. Its effects are the effects of the named function or lambda.
fn callback(program: &Program, body: &Body, call: &Call) -> (Callback, EffectSet) {
    let Some(id) = argument_expr(call, 1) else {
        return (Callback::Dynamic, EffectSet::opaque());
    };
    match &body.exprs[id].kind {
        ExprKind::Def(def) => match &program.defs[*def].kind {
            DefKind::Fn(function) => (Callback::Def(*def), function.effects),
            _ => (Callback::Dynamic, EffectSet::opaque()),
        },
        ExprKind::Lambda { body: lambda, .. } => {
            (Callback::Lambda(*lambda), program.bodies[*lambda].effects)
        }
        _ => (Callback::Dynamic, EffectSet::opaque()),
    }
}

/// A source the walk cannot name: its rows come from an expression that is evaluated, so its effects are unknown.
fn external() -> Operator {
    Operator {
        kind: OperatorKind::External,
        table: None,
        callback: None,
        effects: EffectSet::opaque(),
        inputs: Vec::new(),
    }
}

/// The expression bound to argument `slot`, if the call binds it to a written expression.
fn argument_expr(call: &Call, slot: usize) -> Option<ExprId> {
    let argument = call
        .args
        .iter()
        .find(|argument| argument.slot.as_u32() as usize == slot)?;
    match argument.value {
        ArgValue::Expr(id) => Some(id),
        ArgValue::Default => None,
    }
}
