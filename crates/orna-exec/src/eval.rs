//! Expression evaluation over one body.

use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use orna_hir::{
    Arg, ArgValue, Arm, AssignOp, BinOp, Body, BodyId, Call, Callee, DefId, DefKind, ExprId,
    ExprKind, FieldRef, FnBody, InterpPart, IntrinsicForm, Lit, LocalId, LogicOp, Name, ParamDef,
    Pat, PatId, Place, Stmt,
};

use crate::binders;
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::frame::{Frame, TypeEnv, type_args};
use crate::machine::Machine;
use crate::natives;
use crate::ops;
use crate::relation;
use crate::value::{Closure, Payload, Value};

impl<'p> Machine<'p> {
    /// Evaluates one expression of `body` in `frame`.
    pub(crate) fn eval(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        id: ExprId,
    ) -> Result<Value, Stop> {
        self.tick(body.exprs[id].weight)?;
        match &body.exprs[id].kind {
            ExprKind::Lit(lit) => self.literal(lit),
            ExprKind::Local(slot) => frame.get(*slot),
            // A bare word no pass resolved names nothing at run time, the old pipeline's `ORNA-EVAL-NAME`.
            ExprKind::BareName(_) => Err(Stop::Error("ORNA-EVAL-NAME")),
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
            ExprKind::For {
                pat,
                iter,
                body: loop_body,
            } => self.for_loop(body, frame, *pat, *iter, *loop_body),
            ExprKind::Return(value) => {
                let value = self.optional(body, frame, *value)?;
                Err(Stop::Return(value))
            }
            ExprKind::Break(None) => Err(Stop::Break),
            ExprKind::Break(Some(_)) => Err(Stop::NotYet("break with a value (H4)")),
            ExprKind::Continue => Err(Stop::Continue),
            ExprKind::Assign { place, op, value } => self.assign(body, frame, place, *op, *value),
            ExprKind::Tuple(items) => Ok(Value::Tuple(self.elements(body, frame, items)?)),
            ExprKind::List(items) => Ok(Value::List(self.elements(body, frame, items)?)),
            ExprKind::Record(fields) => self.record(body, frame, fields),
            ExprKind::Field { base, field } => self.field(body, frame, *base, field),
            ExprKind::Index { base, index } => self.index(body, frame, *base, *index),
            ExprKind::Case { scrutinee, arms } => self.case(body, frame, *scrutinee, arms),
            ExprKind::Lambda {
                body: lambda,
                captured: _,
            } => self.lambda(frame, *lambda),
            ExprKind::Recover {
                body: attempt,
                handler,
                arm,
            } => self.recover(body, frame, *attempt, *handler, *arm),
            ExprKind::Def(variant) if self.is_unit_variant(*variant) => self.unit_variant(*variant),
            ExprKind::Def(def) if matches!(self.program.defs[*def].kind, DefKind::Let(_)) => {
                self.global(*def)
            }
            ExprKind::Construct { def, fields }
                if matches!(self.program.defs[*def].kind, DefKind::Type(_)) =>
            {
                self.nominal(body, frame, fields)
            }
            ExprKind::Interp(parts) => self.interpolate(body, frame, parts),
            _ => Err(Stop::NotYet(kind_name(&body.exprs[id].kind))),
        }
    }

    /// Runs an interpolation: each expression part is a `Display` call yielding its text, and the parts concatenate
    /// in order, bounded by the string limit.
    fn interpolate(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        parts: &'p [InterpPart],
    ) -> Result<Value, Stop> {
        let mut text = String::new();
        for part in parts {
            match part {
                InterpPart::Text(literal) => text.push_str(literal),
                InterpPart::Expr(value) => match self.eval(body, frame, *value)? {
                    Value::Text(piece) => text.push_str(&piece),
                    _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
                },
            }
            if text.len() > self.limits.max_string_bytes {
                return Err(Stop::Error("ORNA-EVAL-LIMIT"));
            }
        }
        Ok(Value::Text(text))
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
            Lit::Decimal {
                coefficient,
                exponent,
            } => Decimal::new(coefficient.clone(), exponent.clone()).map(Value::Decimal),
            Lit::Float(bits) => Ok(crate::float::canonical(f64::from_bits(*bits))),
            Lit::Instant {
                seconds,
                nanoseconds,
            } => Ok(Value::Instant {
                unix_seconds: *seconds,
                nanosecond: *nanoseconds,
            }),
            Lit::Duration {
                seconds,
                nanoseconds,
            } => Ok(Value::Duration {
                seconds: ops::checked(seconds.clone(), &self.limits)?,
                nanosecond: *nanoseconds,
            }),
            Lit::None => Ok(Value::Null),
            Lit::Date(days) => Ok(Value::Date(*days)),
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
            LogicOp::Coalesce => return self.coalesce(body, frame, lhs, rhs),
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
        match &call.callee {
            Callee::Source { def, targs } => {
                let program = self.program;
                let DefKind::Fn(function) = &program.defs[*def].kind else {
                    return Err(Stop::NotYet("call of a non-function definition (H4)"));
                };
                let callee_env = self.resolve_targs(targs, frame.env_slice())?;
                let arguments = self.arguments(body, frame, call, &function.params, &callee_env)?;
                let FnBody::Source(callee_body) = function.body else {
                    return self.dispatch(*def, type_args(&callee_env), arguments);
                };
                self.invoke(callee_body, arguments, &[], callee_env)
            }
            Callee::Value { callee } => {
                // A pipe evaluates its piped input before the stage's callee, so a failing input never reaches the
                // callee. A direct call evaluates the callee first.
                let input = match (call.piped, call.args.first()) {
                    (true, Some(first)) => {
                        let env = frame.env();
                        Some(self.argument(body, frame, first, &[], &env)?)
                    }
                    _ => None,
                };
                let Value::Closure(closure) = self.eval(body, frame, *callee)? else {
                    return Err(Stop::NotYet("call of a non-closure value (H4)"));
                };
                let program = self.program;
                let callee_body = &program.bodies[closure.body];
                let arguments =
                    self.closure_arguments(body, frame, call, input, callee_body.params.len())?;
                if arguments.len() != callee_body.params.len()
                    || binders::binds_a_name_twice(callee_body)
                {
                    return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
                }
                self.invoke(
                    closure.body,
                    arguments,
                    &closure.captured,
                    closure.env.clone(),
                )
            }
            Callee::Native { def, targs } | Callee::Sys { def, targs } => {
                let program = self.program;
                let DefKind::Fn(function) = &program.defs[*def].kind else {
                    return Err(Stop::NotYet("call of a non-function definition (H5)"));
                };
                let callee_targs = self.resolve_targs(targs, frame.env_slice())?;
                let arguments =
                    self.arguments(body, frame, call, &function.params, &callee_targs)?;
                let values = natives::in_parameter_order(arguments, function.params.len())?;
                self.native(*def, type_args(&callee_targs), values)
            }
            Callee::TableOp { table, op } => self.table_op(body, frame, call, *table, *op),
            Callee::Relational { op } => relation::relational(self, body, frame, call, *op),
            Callee::Intrinsic(IntrinsicForm::Fail) => self.fail(body, frame, call),
            Callee::Intrinsic(IntrinsicForm::OptionSome) => self.option_some(body, frame, call),
            Callee::Intrinsic(IntrinsicForm::Now) => {
                self.no_argument_intrinsic(call, natives::clock::now)
            }
            Callee::Intrinsic(IntrinsicForm::Uuid7) => {
                self.no_argument_intrinsic(call, natives::clock::uuid7)
            }
            Callee::Intrinsic(_) => Err(Stop::NotYet("intrinsic form (H4)")),
            Callee::Pending { .. } => Err(Stop::NotYet("member call awaiting the type pass (H2)")),
            Callee::Ctor { def } => self.construct(body, frame, call, *def),
        }
    }

    fn arguments(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
        params: &[ParamDef],
        env: &TypeEnv,
    ) -> Result<Vec<(usize, Value)>, Stop> {
        let mut arguments = Vec::with_capacity(call.args.len());
        for argument in call.args.iter() {
            arguments.push(self.argument(body, frame, argument, params, env)?);
        }
        Ok(arguments)
    }

    /// Evaluates the arguments of a closure call in order. `input` is the already-evaluated piped argument 0, when
    /// the call is a pipe. The old evaluator binds positionally and fails at the first argument with no parameter
    /// before evaluating it, so an excess argument is never evaluated.
    fn closure_arguments(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
        input: Option<(usize, Value)>,
        arity: usize,
    ) -> Result<Vec<(usize, Value)>, Stop> {
        let skip = usize::from(input.is_some());
        let env = frame.env();
        let mut arguments: Vec<(usize, Value)> = input.into_iter().collect();
        for (index, argument) in call.args.iter().enumerate().skip(skip) {
            if index >= arity {
                return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
            }
            arguments.push(self.argument(body, frame, argument, &[], &env)?);
        }
        Ok(arguments)
    }

    fn argument(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        argument: &Arg,
        params: &[ParamDef],
        env: &TypeEnv,
    ) -> Result<(usize, Value), Stop> {
        let slot = argument.slot.as_u32() as usize;
        match argument.value {
            ArgValue::Expr(id) => {
                let value = self.eval(body, frame, id)?;
                Ok((slot, value))
            }
            ArgValue::Default => {
                let Some(default) = params.get(slot).and_then(|param| param.default) else {
                    return Err(Stop::NotYet("default argument without a default body (H4)"));
                };
                let value = self.invoke(default, Vec::new(), &[], env.clone())?;
                Ok((slot, value))
            }
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
                    if !self.matches(body, frame, pat, value)? {
                        return Err(Stop::Error("ORNA-EVAL-TYPE"));
                    }
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
        match self.eval(body, frame, cond)? {
            Value::Bool(true) => {
                let value = self.eval(body, frame, then)?;
                Ok(if els.is_some() { value } else { Value::Unit })
            }
            Value::Bool(false) => match els {
                Some(els) => self.eval(body, frame, els),
                None => Ok(Value::Unit),
            },
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

    /// `for` over an integer range literal (`lo..hi` or `lo..=hi`); other
    /// iterables are not yet evaluated.
    fn for_loop(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        pat: PatId,
        iter: ExprId,
        loop_body: ExprId,
    ) -> Result<Value, Stop> {
        let ExprKind::Range {
            lo: Some(lo),
            hi: Some(hi),
            inclusive,
        } = &body.exprs[iter].kind
        else {
            return Err(Stop::NotYet("for over a non-range iterable (H4)"));
        };
        let Value::Int(start) = self.eval(body, frame, *lo)? else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let Value::Int(end) = self.eval(body, frame, *hi)? else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let mut next = start;
        while next < end || (*inclusive && next == end) {
            if !self.matches(body, frame, pat, Value::Int(next.clone()))? {
                return Err(Stop::Error("ORNA-EVAL-TYPE"));
            }
            match self.eval(body, frame, loop_body) {
                Ok(_) | Err(Stop::Continue) => {}
                Err(Stop::Break) => return Ok(Value::Unit),
                Err(stop) => return Err(stop),
            }
            next += BigInt::from(1);
        }
        Ok(Value::Unit)
    }

    fn assign(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        place: &Place,
        op: AssignOp,
        value: ExprId,
    ) -> Result<Value, Stop> {
        match place {
            Place::Local(slot) => {
                let rhs = self.eval(body, frame, value)?;
                let new = self.combine(op, rhs, || frame.get(*slot))?;
                frame.assign(*slot, new)?;
                Ok(Value::Unit)
            }
            Place::Field { base, field } => self.assign_field(body, frame, base, field, op, value),
            Place::Index { base, index } => self.assign_index(body, frame, base, *index, op, value),
        }
    }

    /// Applies an assignment operator. `Set` never reads the current value.
    fn combine(
        &self,
        op: AssignOp,
        rhs: Value,
        current: impl FnOnce() -> Result<Value, Stop>,
    ) -> Result<Value, Stop> {
        let bin = match op {
            AssignOp::Set => return Ok(rhs),
            AssignOp::Add => BinOp::Add,
            AssignOp::Subtract => BinOp::Sub,
            AssignOp::Multiply => BinOp::Mul,
            AssignOp::Divide => BinOp::Div,
        };
        ops::binary(bin, current()?, rhs, &self.limits)
    }

    /// Assigns to a named field of a record held in a local. Deeper places decline.
    fn assign_field(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        base: &Place,
        field: &FieldRef,
        op: AssignOp,
        value: ExprId,
    ) -> Result<Value, Stop> {
        let Place::Local(slot) = base else {
            return Err(Stop::NotYet("assignment to a nested field (H4)"));
        };
        let FieldRef::Name(name) = field else {
            return Err(Stop::NotYet(
                "assignment to a field by index or definition (H4)",
            ));
        };
        let Value::Record(fields) = frame.get(*slot)? else {
            return Err(Stop::NotYet(
                "field assignment on a value that is not a record",
            ));
        };
        let position = fields
            .iter()
            .position(|(key, _)| key == name)
            .ok_or(Stop::Error("ORNA-EVAL-FIELD"))?;
        let rhs = self.eval(body, frame, value)?;
        let new = self.combine(op, rhs, || Ok(fields[position].1.clone()))?;
        let mut updated = fields.to_vec();
        updated[position].1 = new;
        frame.assign(*slot, Value::Record(updated.into()))?;
        Ok(Value::Unit)
    }

    /// Assigns to an element of a list held in a local. Deeper places decline.
    fn assign_index(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        base: &Place,
        index: ExprId,
        op: AssignOp,
        value: ExprId,
    ) -> Result<Value, Stop> {
        let Place::Local(slot) = base else {
            return Err(Stop::NotYet("assignment to a nested index"));
        };
        let Value::List(values) = frame.get(*slot)? else {
            return Err(Stop::NotYet(
                "index assignment on a value that is not a list",
            ));
        };
        let Value::Int(index) = self.eval(body, frame, index)? else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let index = index.to_usize().ok_or(Stop::Error("ORNA-EVAL-INDEX"))?;
        if index >= values.len() {
            return Err(Stop::Error("ORNA-EVAL-INDEX"));
        }
        let rhs = self.eval(body, frame, value)?;
        let new = self.combine(op, rhs, || Ok(values[index].clone()))?;
        let mut updated = values.to_vec();
        updated[index] = new;
        frame.assign(*slot, Value::List(updated.into()))?;
        Ok(Value::Unit)
    }

    /// Evaluates the items of a tuple or list in order.
    fn elements(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        items: &[ExprId],
    ) -> Result<Rc<[Value]>, Stop> {
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(self.eval(body, frame, *item)?);
        }
        Ok(values.into())
    }

    fn record(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        fields: &[(Name, ExprId)],
    ) -> Result<Value, Stop> {
        let mut values = Vec::with_capacity(fields.len());
        for (name, value) in fields {
            values.push((name.clone(), self.eval(body, frame, *value)?));
        }
        Ok(Value::Record(values.into()))
    }

    /// A call of a source enum variant. Its arguments fill the payload fields
    /// by slot; a variant with no fields takes no arguments.
    fn construct(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
        variant: DefId,
    ) -> Result<Value, Stop> {
        let Some(ids) = self.variant_ids_of(variant) else {
            return Err(Stop::NotYet("money or type constructor (H4)"));
        };
        let program = self.program;
        let DefKind::Variant(variant_def) = &program.defs[variant].kind else {
            return Err(Stop::NotYet("money or type constructor (H4)"));
        };
        let env = frame.env();
        let arguments = self.arguments(body, frame, call, &[], &env)?;
        let payload = self.payload(&variant_def.fields, arguments)?;
        Ok(Value::Enum {
            type_id: ids.type_id,
            variant_id: ids.variant_id,
            payload,
        })
    }

    /// Orders constructor arguments into the payload record of a variant. Each
    /// argument fills the field at its slot; every field is filled exactly once.
    /// The record is sorted by field name, as the old pipeline stores it.
    fn payload(&self, fields: &[DefId], arguments: Vec<(usize, Value)>) -> Result<Payload, Stop> {
        if fields.is_empty() {
            return if arguments.is_empty() {
                Ok(None)
            } else {
                Err(Stop::Error("ORNA-EVAL-ARGUMENT"))
            };
        }
        let mut slots: Vec<Option<Value>> = vec![None; fields.len()];
        for (slot, value) in arguments {
            let Some(cell) = slots.get_mut(slot) else {
                return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
            };
            if cell.replace(value).is_some() {
                return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
            }
        }
        let mut record = Vec::with_capacity(fields.len());
        for (field, slot) in fields.iter().zip(slots) {
            let Some(value) = slot else {
                return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
            };
            record.push((self.program.defs[*field].name.clone(), value));
        }
        record.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(Some(record.into()))
    }

    /// A constructor pattern: the value must be the same variant, and each field
    /// pattern must match the payload field it names. A pattern with no field
    /// patterns matches only a payload-free variant.
    fn matches_variant(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        variant: DefId,
        fields: &[(DefId, PatId)],
        value: Value,
    ) -> Result<bool, Stop> {
        let Some(ids) = self.variant_ids_of(variant) else {
            return Err(Stop::NotYet("money or type constructor pattern (H4)"));
        };
        let Value::Enum {
            type_id,
            variant_id,
            payload,
        } = value
        else {
            return Ok(false);
        };
        if type_id != ids.type_id || variant_id != ids.variant_id {
            return Ok(false);
        }
        match (fields.is_empty(), payload) {
            (true, None) => Ok(true),
            (true, Some(_)) | (false, None) => Ok(false),
            (false, Some(record)) => {
                let program = self.program;
                for (field, pat) in fields {
                    let name = &program.defs[*field].name;
                    let Some((_, value)) = record.iter().find(|(key, _)| key == name) else {
                        return Ok(false);
                    };
                    if !self.matches(body, frame, *pat, value.clone())? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    /// A nominal struct value: its fields by name, in name order like a structural record.
    fn nominal(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        fields: &[(DefId, ExprId)],
    ) -> Result<Value, Stop> {
        let mut values = Vec::with_capacity(fields.len());
        for (field, value) in fields {
            let name = self.program.defs[*field].name.clone();
            values.push((name, self.eval(body, frame, *value)?));
        }
        values.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(Value::Record(values.into()))
    }

    /// `lhs ?? rhs`: `lhs` unless it is absent, then `rhs`.
    fn coalesce(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Value, Stop> {
        match self.eval(body, frame, lhs)? {
            Value::Null => self.eval(body, frame, rhs),
            value => Ok(value),
        }
    }

    fn field(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        base: ExprId,
        field: &FieldRef,
    ) -> Result<Value, Stop> {
        let Value::Record(fields) = self.eval(body, frame, base)? else {
            return Err(Stop::NotYet(
                "field access on a value the type pass rejects (H2)",
            ));
        };
        let found = match field {
            FieldRef::Index(index) => fields.get(*index as usize).map(|(_, value)| value),
            FieldRef::Name(name) => fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value),
            FieldRef::Def(def) => {
                let name = &self.program.defs[*def].name;
                fields
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value)
            }
        };
        found.cloned().ok_or(Stop::Error("ORNA-EVAL-FIELD"))
    }

    fn index(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        base: ExprId,
        index: ExprId,
    ) -> Result<Value, Stop> {
        let base = self.eval(body, frame, base)?;
        let index = self.eval(body, frame, index)?;
        let (Value::List(values) | Value::Tuple(values)) = base else {
            return Err(Stop::NotYet(
                "index access on a value that is not a list or tuple",
            ));
        };
        let Value::Int(index) = index else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let index = index.to_usize().ok_or(Stop::Error("ORNA-EVAL-INDEX"))?;
        values
            .get(index)
            .cloned()
            .ok_or(Stop::Error("ORNA-EVAL-INDEX"))
    }

    fn case(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        scrutinee: ExprId,
        arms: &[Arm],
    ) -> Result<Value, Stop> {
        let value = self.eval(body, frame, scrutinee)?;
        for arm in arms {
            if !self.matches(body, frame, arm.pat, value.clone())? {
                continue;
            }
            if let Some(guard) = arm.guard {
                match self.eval(body, frame, guard)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) => continue,
                    _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
                }
            }
            return self.eval(body, frame, arm.body);
        }
        Err(Stop::Error("ORNA-EVAL-NO-MATCH"))
    }

    /// Builds a closure over the variables its body captures. The closure shares
    /// each variable's slot, so an assignment on either side is seen by the other.
    fn lambda(&self, frame: &mut Frame, lambda: BodyId) -> Result<Value, Stop> {
        let program = self.program;
        let captures = &program.bodies[lambda].captures;
        let mut captured = Vec::with_capacity(captures.len());
        for capture in captures.iter() {
            captured.push(frame.cell(capture.outer)?);
        }
        Ok(Value::Closure(Rc::new(Closure {
            body: lambda,
            captured: captured.into_boxed_slice(),
            env: frame.env(),
        })))
    }

    /// `attempt |? handler`: a failure of `attempt` reaches the handler as an
    /// error value. A stop that is not a language error is not a failure and
    /// passes through.
    fn recover(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        attempt: ExprId,
        handler: LocalId,
        arm: ExprId,
    ) -> Result<Value, Stop> {
        match self.eval(body, frame, attempt) {
            Err(Stop::Error(code)) => {
                frame.bind(handler, Value::Error(code))?;
                self.eval(body, frame, arm)
            }
            other => other,
        }
    }

    /// `fail(e)`: raises the error value `e` names, which a recovery handler
    /// binds. Any other value is a type error, as in the old pipeline. The
    /// argument is the only input; a defaulted or missing argument is an
    /// argument error.
    fn fail(&mut self, body: &'p Body, frame: &mut Frame, call: &Call) -> Result<Value, Stop> {
        let argument = Self::sole_argument(call)?;
        match self.eval(body, frame, argument)? {
            Value::Error(code) => Err(Stop::Error(code)),
            _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
        }
    }

    /// A zero-argument intrinsic form: the clock and entropy reads. Any argument is an argument error.
    fn no_argument_intrinsic(
        &mut self,
        call: &Call,
        read: fn(&mut Self) -> Result<Value, Stop>,
    ) -> Result<Value, Stop> {
        if !call.args.is_empty() {
            return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
        }
        read(self)
    }

    /// `Some(e)`: the present option holding the value of `e`. The call takes exactly one argument; a defaulted or
    /// missing argument is an argument error.
    fn option_some(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
    ) -> Result<Value, Stop> {
        let argument = Self::sole_argument(call)?;
        Ok(Value::Option(Some(Rc::new(
            self.eval(body, frame, argument)?,
        ))))
    }

    /// The one expression argument of a single-argument intrinsic form. A defaulted or missing argument is an
    /// argument error.
    fn sole_argument(call: &Call) -> Result<ExprId, Stop> {
        let [
            Arg {
                value: ArgValue::Expr(argument),
                ..
            },
        ] = &call.args[..]
        else {
            return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
        };
        Ok(*argument)
    }

    /// Binds `value` to `pat` when it matches. Bindings made before a later
    /// mismatch stay in their slots; a pattern binds only its own slots, which
    /// no other expression reads before the arm that owns them.
    fn matches(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        pat: PatId,
        value: Value,
    ) -> Result<bool, Stop> {
        match &body.pats[pat] {
            Pat::Wild => Ok(true),
            Pat::Bind { local } => {
                frame.bind(*local, value)?;
                Ok(true)
            }
            Pat::Lit(lit) => Ok(self.literal(lit)? == value),
            Pat::Tuple(pats) => match value {
                Value::Tuple(values) if values.len() == pats.len() => {
                    self.matches_seq(body, frame, pats, &values)
                }
                _ => Ok(false),
            },
            Pat::List {
                prefix,
                rest,
                suffix,
            } => match value {
                Value::List(values) => {
                    self.matches_list(body, frame, prefix, *rest, suffix, &values)
                }
                _ => Ok(false),
            },
            Pat::Record(fields) => match value {
                Value::Record(values) => self.matches_record(body, frame, fields, &values),
                _ => Ok(false),
            },
            Pat::Ctor { def, fields } => self.matches_variant(body, frame, *def, fields, value),
            Pat::Some(inner) => match value {
                Value::Null => Ok(false),
                _ => self.matches(body, frame, *inner, value),
            },
        }
    }

    /// Matches each pattern against the value at the same position.
    fn matches_seq(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        pats: &[PatId],
        values: &[Value],
    ) -> Result<bool, Stop> {
        for (pat, value) in pats.iter().zip(values) {
            if !self.matches(body, frame, *pat, value.clone())? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A list pattern: prefix, then an optional rest binding, then suffix.
    /// Without a rest binding the length must be exact.
    fn matches_list(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        prefix: &[PatId],
        rest: Option<LocalId>,
        suffix: &[PatId],
        values: &[Value],
    ) -> Result<bool, Stop> {
        let fixed = prefix.len() + suffix.len();
        let length_fits = match rest {
            None => values.len() == fixed,
            Some(_) => values.len() >= fixed,
        };
        if !length_fits {
            return Ok(false);
        }
        let suffix_start = values.len() - suffix.len();
        if !self.matches_seq(body, frame, prefix, &values[..prefix.len()])?
            || !self.matches_seq(body, frame, suffix, &values[suffix_start..])?
        {
            return Ok(false);
        }
        if let Some(rest) = rest {
            let middle = Value::List(values[prefix.len()..suffix_start].into());
            frame.bind(rest, middle)?;
        }
        Ok(true)
    }

    /// A record pattern: each named field must exist and match.
    fn matches_record(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        fields: &[(Name, PatId)],
        values: &[(Name, Value)],
    ) -> Result<bool, Stop> {
        for (name, pat) in fields {
            let Some((_, value)) = values.iter().find(|(key, _)| key == name) else {
                return Ok(false);
            };
            if !self.matches(body, frame, *pat, value.clone())? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The name of an expression kind, for the not-yet report.
fn kind_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Def(_) => "value use of a definition",
        ExprKind::Range { .. } => "range",
        ExprKind::Construct { .. } => "construction",
        ExprKind::Interp(_) => "interpolation",
        ExprKind::For { .. } => "for",
        ExprKind::Loop { .. } => "loop",
        ExprKind::Error => "error expression",
        _ => "expression kind",
    }
}
