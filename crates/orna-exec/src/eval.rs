//! Expression evaluation over one body.

use orna_hir::{
    Arg, ArgValue, AssignOp, BinOp, Body, Call, Callee, DefKind, ExprId, ExprKind, FnBody, Lit,
    LogicOp, Pat, PatId, Place, Stmt,
};

use crate::error::Stop;
use crate::frame::Frame;
use crate::machine::Machine;
use crate::ops;
use crate::value::Value;

impl<'p> Machine<'p> {
    /// Evaluates one expression of `body` in `frame`.
    pub(crate) fn eval(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        id: ExprId,
    ) -> Result<Value, Stop> {
        self.tick()?;
        match &body.exprs[id].kind {
            ExprKind::Lit(lit) => self.literal(lit),
            ExprKind::Local(slot) => frame.get(*slot),
            ExprKind::Unary { op, operand } => {
                let value = self.eval(body, frame, *operand)?;
                ops::unary(*op, value)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.eval(body, frame, *lhs)?;
                let rhs = self.eval(body, frame, *rhs)?;
                ops::binary(*op, lhs, rhs, &self.limits)
            }
            ExprKind::Logic { op, lhs, rhs } => self.logic(body, frame, *op, *lhs, *rhs),
            ExprKind::Call(call) => self.call(body, frame, call),
            ExprKind::Block { stmts, tail } => self.block(body, frame, stmts, *tail),
            ExprKind::If { cond, then, els } => self.conditional(body, frame, *cond, *then, *els),
            ExprKind::While {
                cond,
                body: loop_body,
            } => self.while_loop(body, frame, *cond, *loop_body),
            ExprKind::Return(value) => {
                let value = self.optional(body, frame, *value)?;
                Err(Stop::Return(value))
            }
            ExprKind::Break(None) => Err(Stop::Break),
            ExprKind::Break(Some(_)) => Err(Stop::NotYet("break with a value")),
            ExprKind::Continue => Err(Stop::Continue),
            ExprKind::Assign { place, op, value } => self.assign(body, frame, place, *op, *value),
            _ => Err(Stop::NotYet(kind_name(&body.exprs[id].kind))),
        }
    }

    fn literal(&self, lit: &Lit) -> Result<Value, Stop> {
        match lit {
            Lit::Int(value) => {
                if value.to_str_radix(10).len() > self.limits.max_integer_digits {
                    return Err(Stop::Error("ORNA-EVAL-LIMIT"));
                }
                Ok(Value::Int(value.clone()))
            }
            Lit::Bool(value) => Ok(Value::Bool(*value)),
            Lit::Str(text) => {
                if text.len() > self.limits.max_string_bytes {
                    return Err(Stop::Error("ORNA-EVAL-LIMIT"));
                }
                Ok(Value::Text(text.to_string()))
            }
            Lit::Decimal { .. }
            | Lit::Float(_)
            | Lit::None
            | Lit::Date(_)
            | Lit::Instant { .. }
            | Lit::Period(_)
            | Lit::Duration { .. } => Err(Stop::NotYet("literal kind")),
        }
    }

    fn optional(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        id: Option<ExprId>,
    ) -> Result<Value, Stop> {
        match id {
            Some(id) => self.eval(body, frame, id),
            None => Ok(Value::Unit),
        }
    }

    fn logic(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        op: LogicOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Value, Stop> {
        let short_circuit = match op {
            LogicOp::And => false,
            LogicOp::Or => true,
            LogicOp::Coalesce => return Err(Stop::NotYet("coalesce operator")),
        };
        match self.eval(body, frame, lhs)? {
            Value::Bool(value) if value == short_circuit => Ok(Value::Bool(value)),
            Value::Bool(_) => match self.eval(body, frame, rhs)? {
                Value::Bool(value) => Ok(Value::Bool(value)),
                _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
            },
            _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
        }
    }

    fn call(&mut self, body: &'p Body, frame: &mut Frame, call: &Call) -> Result<Value, Stop> {
        let Callee::Source { def, .. } = &call.callee else {
            return Err(Stop::NotYet("call of a non-source callee"));
        };
        let program = self.program;
        let DefKind::Fn(function) = &program.defs[*def].kind else {
            return Err(Stop::NotYet("call of a non-function definition"));
        };
        let FnBody::Source(callee_body) = function.body else {
            return Err(Stop::NotYet("call of a function without a source body"));
        };
        let mut arguments = Vec::with_capacity(call.args.len());
        for argument in call.args.iter() {
            arguments.push(self.argument(body, frame, argument)?);
        }
        self.invoke(callee_body, arguments)
    }

    fn argument(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        argument: &Arg,
    ) -> Result<(usize, Value), Stop> {
        match argument.value {
            ArgValue::Expr(id) => {
                let value = self.eval(body, frame, id)?;
                Ok((argument.slot.as_u32() as usize, value))
            }
            ArgValue::Default => Err(Stop::NotYet("default argument")),
        }
    }

    fn block(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        stmts: &[Stmt],
        tail: Option<ExprId>,
    ) -> Result<Value, Stop> {
        for stmt in stmts {
            match *stmt {
                Stmt::Let { pat, init } => {
                    let value = self.eval(body, frame, init)?;
                    bind(body, frame, pat, value)?;
                }
                Stmt::Assert(id) => match self.eval(body, frame, id)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) => return Err(Stop::Error("ORNA-A091-008")),
                    _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
                },
                Stmt::Expr(id) => {
                    self.eval(body, frame, id)?;
                }
            }
        }
        self.optional(body, frame, tail)
    }

    fn conditional(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        cond: ExprId,
        then: ExprId,
        els: Option<ExprId>,
    ) -> Result<Value, Stop> {
        let Some(els) = els else {
            return Err(Stop::NotYet("if without else"));
        };
        match self.eval(body, frame, cond)? {
            Value::Bool(true) => self.eval(body, frame, then),
            Value::Bool(false) => self.eval(body, frame, els),
            _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
        }
    }

    fn while_loop(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        cond: ExprId,
        loop_body: ExprId,
    ) -> Result<Value, Stop> {
        loop {
            match self.eval(body, frame, cond)? {
                Value::Bool(true) => {}
                Value::Bool(false) => return Ok(Value::Unit),
                _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
            }
            match self.eval(body, frame, loop_body) {
                Ok(_) | Err(Stop::Continue) => {}
                Err(Stop::Break) => return Ok(Value::Unit),
                Err(stop) => return Err(stop),
            }
        }
    }

    fn assign(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        place: &Place,
        op: AssignOp,
        value: ExprId,
    ) -> Result<Value, Stop> {
        let Place::Local(slot) = place else {
            return Err(Stop::NotYet("assignment to a field or index"));
        };
        let rhs = self.eval(body, frame, value)?;
        let new = match op {
            AssignOp::Set => rhs,
            AssignOp::Add => ops::binary(BinOp::Add, frame.get(*slot)?, rhs, &self.limits)?,
            AssignOp::Subtract => ops::binary(BinOp::Sub, frame.get(*slot)?, rhs, &self.limits)?,
            AssignOp::Multiply => ops::binary(BinOp::Mul, frame.get(*slot)?, rhs, &self.limits)?,
            AssignOp::Divide => ops::binary(BinOp::Div, frame.get(*slot)?, rhs, &self.limits)?,
        };
        frame.set(*slot, new)?;
        Ok(Value::Unit)
    }
}

/// Binds a pattern this stage runs: a wildcard or a plain binding.
fn bind(body: &Body, frame: &mut Frame, pat: PatId, value: Value) -> Result<(), Stop> {
    match &body.pats[pat] {
        Pat::Wild => Ok(()),
        Pat::Bind { local } => frame.set(*local, value),
        _ => Err(Stop::NotYet("pattern kind")),
    }
}

/// The name of an expression kind, for the not-yet report.
fn kind_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Def(_) => "value use of a definition",
        ExprKind::Range { .. } => "range",
        ExprKind::Recover { .. } => "recover",
        ExprKind::Field { .. } => "field access",
        ExprKind::Index { .. } => "index access",
        ExprKind::Tuple(_) => "tuple",
        ExprKind::List(_) => "list",
        ExprKind::Record(_) => "record",
        ExprKind::Construct { .. } => "construction",
        ExprKind::Lambda { .. } => "lambda",
        ExprKind::Interp(_) => "interpolation",
        ExprKind::Case { .. } => "case",
        ExprKind::For { .. } => "for",
        ExprKind::Loop { .. } => "loop",
        ExprKind::Error => "error expression",
        _ => "expression kind",
    }
}
