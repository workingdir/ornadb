//! Call checking: source/native/sys, constructors, first-class values, table
//! operations, relation stages and intrinsic forms.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{
    Arg, ArgValue, Body, BodyId, Callee, DefId, DefKind, ExprId, ExprKind, FieldRef, FnBody, FnTy,
    IntrinsicForm, Name, Place, RelOp, TableOp, Ty, TyKind,
};

use super::{BodyCheck, Container, NotYetReason, relation_operation, table_operation};

impl<'a> BodyCheck<'a> {
    pub(super) fn check_lambda(
        &mut self,
        child: BodyId,
        expected: Option<FnTy>,
        parent: &Body,
    ) -> Result<Ty, NotYetReason> {
        let mut body = self.take(child);
        let captures = body.captures.clone();
        for capture in captures {
            let outer = parent.locals[capture.outer].ty;
            body.locals[capture.inner].ty = self.resolved(outer);
        }
        let params = body.params.clone();
        for (index, local) in params.iter().enumerate() {
            let current = body.locals[*local].ty;
            let ty = if matches!(self.program.tys.kind(current), Some(TyKind::Error)) {
                match expected
                    .as_ref()
                    .and_then(|function| function.params.get(index))
                {
                    Some(ty) => *ty,
                    None => {
                        let span = body.locals[*local].span.clone();
                        self.infer.new_var(&mut self.program.tys, span)
                    }
                }
            } else {
                current
            };
            body.locals[*local].ty = ty;
        }
        self.prepare_locals(&mut body, true);
        let root = body.root;
        let expected_ret = expected.as_ref().map(|function| function.ret);
        self.returns.push(expected_ret);
        let result = self.infer_expr(&mut body, root, expected_ret);
        self.returns.pop();
        let ret = result?;
        let params = params
            .iter()
            .map(|local| self.resolved(body.locals[*local].ty))
            .collect::<Vec<_>>();
        self.put(child, body);
        Ok(self.intern(TyKind::Fn(FnTy {
            params: params.into_boxed_slice(),
            ret,
            effects: crate::EffectRow::Hidden,
        })))
    }

    /// Types a `Callee` and checks its arguments.
    pub(super) fn call_ty(
        &mut self,
        body: &mut Body,
        call: &mut crate::Call,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let pending = match &call.callee {
            Callee::Pending { base, name } => Some((*base, name.clone())),
            _ => None,
        };
        let mut protocol_member = false;
        if let Some((base, name)) = pending {
            let Some(settled) = self.settle_pending(body, base, &name, span.clone())? else {
                return Ok(self.error());
            };
            if matches!(settled, Callee::Source { .. }) {
                // A protocol member takes the receiver as its first argument (D1: the receiver is not written out).
                call.args = with_receiver(&call.args, base);
                protocol_member = true;
            } else if matches!(settled, Callee::Native { .. }) {
                // `Decimal.divide` is the native `core.decimal.divide`: the receiver is its dividend, argument 0.
                call.args = with_receiver(&call.args, base);
            } else if matches!(settled, Callee::Relational { .. }) {
                // A relation stage takes its receiver as argument 0, as a pipe does: `rows.take(n)` is `rows | take(n)`.
                call.args = with_receiver(&call.args, base);
                call.piped = true;
            }
            call.callee = settled;
        }
        self.record_inferred_targs(call, span.clone());
        match &call.callee {
            Callee::Source { def, targs } => {
                self.source_call_ty(body, call, *def, targs, span, protocol_member)
            }
            Callee::Native { def, targs } | Callee::Sys { def, targs } => {
                self.source_call_ty(body, call, *def, targs, span, false)
            }
            Callee::TableOp { table, op } => {
                let table = *table;
                let op = *op;
                self.table_op_ty(body, call, table, op, span)
            }
            Callee::Relational { op } => {
                let op = *op;
                self.relational_ty(body, call, op, span)
            }
            Callee::Intrinsic(form) => {
                let form = *form;
                self.intrinsic_ty(body, call, form, span)
            }
            Callee::Ctor { def } => {
                let def = *def;
                self.ctor_call_ty(body, call, def, span)
            }
            Callee::Value { callee } => {
                let callee = *callee;
                if let ExprKind::Lambda { body: child, .. } = body.exprs[callee].kind {
                    return self.applied_lambda_ty(body, callee, child, &call.args, span);
                }
                let callee_ty = self.infer_expr(body, callee, None)?;
                let resolved = self.resolved(callee_ty);
                let Some(TyKind::Fn(function)) = self.kind_of(resolved) else {
                    if matches!(self.kind_of(resolved), Some(TyKind::Error)) {
                        return Ok(self.error());
                    }
                    self.conflict("the callee is not a function value", span.clone());
                    return Ok(self.error());
                };
                for (index, argument) in call.args.iter().enumerate() {
                    let Some(expected) = function.params.get(index) else {
                        self.conflict(
                            "the call supplies more arguments than parameters",
                            span.clone(),
                        );
                        continue;
                    };
                    self.check_argument(body, argument, *expected, span.clone())?;
                }
                Ok(function.ret)
            }
            Callee::Pending { .. } => Err(NotYetReason::Unsupported(
                "an unsettled member call is not on HIR".into(),
            )),
        }
    }

    /// D1/D5: a generic call that omits its type arguments gets one inference variable per generic parameter. The
    /// variables are recorded in the callee, so the finishing pass replaces them with the solved types.
    fn record_inferred_targs(&mut self, call: &mut crate::Call, span: SyntaxSpan) {
        let (def, targs) = match &mut call.callee {
            Callee::Source { def, targs }
            | Callee::Native { def, targs }
            | Callee::Sys { def, targs } => (*def, targs),
            _ => return,
        };
        if !targs.is_empty() {
            return;
        }
        let count = match self.program.defs.get(def).map(|d| &d.kind) {
            Some(DefKind::Fn(function)) => function.generics.len(),
            _ => return,
        };
        if count == 0 {
            return;
        }
        *targs = (0..count)
            .map(|_| self.infer.new_var(&mut self.program.tys, span.clone()))
            .collect();
    }

    /// Replaces solved inference variables in the type arguments of a generic callee.
    pub(super) fn zonk_callee_targs(&mut self, callee: &mut Callee) {
        if let Callee::Source { targs, .. }
        | Callee::Native { targs, .. }
        | Callee::Sys { targs, .. } = callee
        {
            for targ in targs.iter_mut() {
                *targ = self.zonk(*targ);
            }
        }
    }

    /// Settles a member call whose meaning the receiver's type decides.
    ///
    /// `Ok(None)` means the selection was ambiguous and the conflict is recorded.
    fn settle_pending(
        &mut self,
        body: &mut Body,
        base: ExprId,
        name: &Name,
        span: SyntaxSpan,
    ) -> Result<Option<Callee>, NotYetReason> {
        let base_ty = self.infer_expr(body, base, None)?;
        let resolved = self.resolved(base_ty);
        if let Some(TyKind::Relation(inner)) = self.kind_of(resolved) {
            if let Some(TyKind::Adt(table, _)) = self.kind_of(inner)
                && matches!(self.program.defs[table].kind, DefKind::Table(_))
                && let Some(op) = table_operation(name.as_str())
            {
                return Ok(Some(Callee::TableOp { table, op }));
            }
            if let Some(op) = relation_operation(name.as_str()) {
                return Ok(Some(Callee::Relational { op }));
            }
        }
        if name.as_str() == "divide" && matches!(self.kind_of(resolved), Some(TyKind::Decimal)) {
            return self.decimal_divide(name).map(Some);
        }
        self.settle_protocol_call(resolved, name, span)
    }

    /// `Decimal.divide` is the engine native `core.decimal.divide`; the receiver is its dividend (argument 0).
    fn decimal_divide(&self, name: &Name) -> Result<Callee, NotYetReason> {
        let path = crate::ModulePath::new([Name::new("core"), Name::new("decimal")]);
        let def = self
            .program
            .modules
            .iter()
            .find(|module| module.path == path)
            .and_then(|module| module.exports.get(name).copied())
            .ok_or_else(|| {
                NotYetReason::Unsupported("the core.decimal module is not loaded".into())
            })?;
        Ok(Callee::Native {
            def,
            targs: Box::new([]),
        })
    }

    /// Types a source, native or `sys` call against the resolved signature.
    fn source_call_ty(
        &mut self,
        body: &mut Body,
        call: &crate::Call,
        def: DefId,
        targs: &[Ty],
        span: SyntaxSpan,
        protocol_member: bool,
    ) -> Result<Ty, NotYetReason> {
        let Some(DefKind::Fn(function)) = self.program.defs.get(def).map(|d| d.kind.clone()) else {
            return Err(NotYetReason::Unsupported(
                "the callee is not a function declaration".into(),
            ));
        };
        // A bodiless function is only callable as a protocol member, whose signature is the call's type.
        if !protocol_member && matches!(function.body, FnBody::Signature) {
            return Err(NotYetReason::Unsupported(
                "calling a function declared without a body is not on HIR yet".into(),
            ));
        }
        if !function.generics.is_empty() && targs.len() != function.generics.len() {
            self.conflict(
                "the call supplies the wrong number of type arguments",
                span.clone(),
            );
            return Ok(self.error());
        }
        let mut params = Vec::with_capacity(function.params.len());
        for parameter in &function.params {
            if matches!(self.program.tys.kind(parameter.ty), Some(TyKind::Error)) {
                return Err(NotYetReason::UnannotatedParameter(def));
            }
            params.push(self.substitute(parameter.ty, targs));
        }
        let ret = self.substitute(function.ret, targs);
        if matches!(self.program.tys.kind(ret), Some(TyKind::Error)) {
            return Err(NotYetReason::Unsupported(
                "the callee's result type is not on HIR yet".into(),
            ));
        }
        for (index, argument) in call.args.iter().enumerate() {
            let Some(expected) = params.get(index).copied() else {
                self.conflict(
                    "the call supplies more arguments than parameters",
                    span.clone(),
                );
                continue;
            };
            self.check_argument(body, argument, expected, span.clone())?;
        }
        Ok(ret)
    }

    /// Replaces a generic parameter with a written type argument.
    pub(super) fn substitute(&mut self, ty: Ty, targs: &[Ty]) -> Ty {
        let Some(kind) = self.kind_of(ty) else {
            return ty;
        };
        let rebuilt = match kind {
            TyKind::Param(index) => {
                return targs.get(index.as_u32() as usize).copied().unwrap_or(ty);
            }
            TyKind::Option(inner) => TyKind::Option(self.substitute(inner, targs)),
            TyKind::List(inner) => TyKind::List(self.substitute(inner, targs)),
            TyKind::Set(inner) => TyKind::Set(self.substitute(inner, targs)),
            TyKind::Range(inner) => TyKind::Range(self.substitute(inner, targs)),
            TyKind::Relation(inner) => TyKind::Relation(self.substitute(inner, targs)),
            TyKind::Stream(inner) => TyKind::Stream(self.substitute(inner, targs)),
            TyKind::Map(key, value) => {
                TyKind::Map(self.substitute(key, targs), self.substitute(value, targs))
            }
            TyKind::Tuple(elements) => TyKind::Tuple(
                elements
                    .iter()
                    .map(|element| self.substitute(*element, targs))
                    .collect(),
            ),
            TyKind::Record(fields) => TyKind::Record(
                fields
                    .iter()
                    .map(|(name, field)| (name.clone(), self.substitute(*field, targs)))
                    .collect(),
            ),
            TyKind::Adt(def, arguments) => TyKind::Adt(
                def,
                arguments
                    .iter()
                    .map(|argument| self.substitute(*argument, targs))
                    .collect(),
            ),
            TyKind::Fn(function) => TyKind::Fn(FnTy {
                params: function
                    .params
                    .iter()
                    .map(|parameter| self.substitute(*parameter, targs))
                    .collect(),
                ret: self.substitute(function.ret, targs),
                effects: function.effects,
            }),
            _ => return ty,
        };
        self.intern(rebuilt)
    }

    fn check_argument(
        &mut self,
        body: &mut Body,
        argument: &Arg,
        expected: Ty,
        _span: SyntaxSpan,
    ) -> Result<(), NotYetReason> {
        if let ArgValue::Expr(id) = argument.value {
            self.infer_expr(body, id, Some(expected))?;
        }
        Ok(())
    }

    /// Types a lambda applied at once (`(x => e)(a)`, or a piped `a | (x => e)`). Each parameter is checked
    /// against its declared type or, when unannotated, against the argument it receives, so the body sees the
    /// argument's type before it is checked. The lambda's own return type is the call's type.
    fn applied_lambda_ty(
        &mut self,
        body: &mut Body,
        callee: ExprId,
        child: BodyId,
        arguments: &[Arg],
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let params = self.lambda_param_tys(child);
        if arguments.len() > params.len() {
            self.conflict(
                "the call supplies more arguments than parameters",
                span.clone(),
            );
        }
        for (argument, param) in arguments.iter().zip(params.iter()) {
            self.check_argument(body, argument, *param, span.clone())?;
        }
        let ret = self.infer.new_var(&mut self.program.tys, span.clone());
        let function = self.intern(TyKind::Fn(FnTy {
            params: params.into_boxed_slice(),
            ret,
            effects: crate::EffectRow::Hidden,
        }));
        let lambda_ty = self.infer_expr(body, callee, Some(function))?;
        let resolved = self.resolved(lambda_ty);
        match self.kind_of(resolved) {
            Some(TyKind::Fn(lambda)) => Ok(lambda.ret),
            _ => Ok(self.error()),
        }
    }

    /// The parameter types of a lambda body: its annotation, or a fresh variable where it omits one.
    fn lambda_param_tys(&mut self, child: BodyId) -> Vec<Ty> {
        let lambda = &self.program.bodies[child];
        let declared: Vec<(Ty, SyntaxSpan)> = lambda
            .params
            .iter()
            .map(|local| (lambda.locals[*local].ty, lambda.locals[*local].span.clone()))
            .collect();
        declared
            .into_iter()
            .map(|(ty, span)| {
                if matches!(self.program.tys.kind(ty), Some(TyKind::Error)) {
                    self.infer.new_var(&mut self.program.tys, span)
                } else {
                    ty
                }
            })
            .collect()
    }

    /// Types one table operation result and its arguments.
    fn table_op_ty(
        &mut self,
        body: &mut Body,
        call: &crate::Call,
        table: DefId,
        op: TableOp,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let row = self.intern(TyKind::Adt(table, Box::new([])));
        let result = match op {
            TableOp::Insert | TableOp::Upsert | TableOp::Update | TableOp::Rekey => row,
            TableOp::Delete => self.intern(TyKind::Unit),
            TableOp::Count => self.intern(TyKind::Int),
            TableOp::First => self.intern(TyKind::Option(row)),
            TableOp::One => row,
            TableOp::AsOf => self.intern(TyKind::Relation(row)),
        };
        match op {
            TableOp::Insert | TableOp::Upsert => {
                self.check_table_row(body, call, 0, table, row, op)?;
            }
            TableOp::Update => {
                let key = self.table_key_ty(table)?;
                self.check_slot(body, call, 0, Some(key), span.clone())?;
                self.check_table_row(body, call, 1, table, row, op)?;
            }
            TableOp::Delete => {
                let key = self.table_key_ty(table)?;
                self.check_slot(body, call, 0, Some(key), span.clone())?;
            }
            TableOp::Rekey => self.check_rekey(body, call, table, span.clone())?,
            TableOp::Count | TableOp::First | TableOp::One => {}
            TableOp::AsOf => {
                self.check_slot(body, call, 0, None, span.clone())?;
            }
        }
        Ok(result)
    }

    /// The type of a table's key: one key field or a tuple of them.
    fn table_key_ty(&mut self, table: DefId) -> Result<Ty, NotYetReason> {
        let keys = match self.program.defs.get(table).map(|def| def.kind.clone()) {
            Some(DefKind::Table(definition)) => definition.key,
            _ => Vec::new(),
        };
        match keys.as_slice() {
            [field] => self.field_definition_ty(*field),
            [] => Err(NotYetReason::Unsupported(
                "a table without a declared key has no H2a key type".into(),
            )),
            many => {
                let mut types = Vec::with_capacity(many.len());
                for field in many {
                    types.push(self.field_definition_ty(*field)?);
                }
                Ok(self.intern(TyKind::Tuple(types.into_boxed_slice())))
            }
        }
    }

    fn check_slot(
        &mut self,
        body: &mut Body,
        call: &crate::Call,
        slot: usize,
        expected: Option<Ty>,
        _span: SyntaxSpan,
    ) -> Result<(), NotYetReason> {
        let Some(argument) = call.args.get(slot) else {
            return Ok(());
        };
        if let ArgValue::Expr(id) = argument.value {
            self.infer_expr(body, id, expected)?;
        }
        Ok(())
    }

    /// Types one relation stage or terminal.
    fn relational_ty(
        &mut self,
        body: &mut Body,
        call: &crate::Call,
        op: RelOp,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        self.relational_on_container(body, call, op, span)
    }

    fn relational_on_container(
        &mut self,
        body: &mut Body,
        call: &crate::Call,
        op: RelOp,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let input = self.argument_expr(call, 0).ok_or_else(|| {
            NotYetReason::Unsupported(format!("`{:?}` needs a collection argument", op).into())
        })?;
        let input_ty = self.infer_expr(body, input, None)?;
        let resolved = self.resolved(input_ty);
        let Some((container, element)) = self.container(resolved) else {
            if matches!(self.kind_of(resolved), Some(TyKind::Error)) {
                return Ok(self.error());
            }
            return Err(NotYetReason::Unsupported(
                "the relation stage input is not a list, relation or stream in H2a".into(),
            ));
        };
        match op {
            RelOp::Count => Ok(self.intern(TyKind::Int)),
            RelOp::First | RelOp::Last => Ok(self.intern(TyKind::Option(element))),
            RelOp::One => Ok(element),
            RelOp::Sum => match self.kind_of(self.resolved(element)) {
                Some(TyKind::Int | TyKind::Decimal | TyKind::Float) => Ok(element),
                Some(TyKind::Error) => Ok(self.error()),
                _ => Err(NotYetReason::Unsupported(
                    "summation of this element type is not on HIR in H2a".into(),
                )),
            },
            RelOp::Min | RelOp::Max => Ok(self.intern(TyKind::Option(element))),
            RelOp::Distinct | RelOp::Unique => Ok(self.wrap(container, element)),
            RelOp::Filter => {
                let bool_ty = self.intern(TyKind::Bool);
                let callback = self.intern(TyKind::Fn(FnTy {
                    params: Box::new([element]),
                    ret: bool_ty,
                    effects: crate::EffectRow::Hidden,
                }));
                self.check_slot(body, call, 1, Some(callback), span)?;
                Ok(self.wrap(container, element))
            }
            RelOp::Map | RelOp::Project => {
                let output = self.infer.new_var(&mut self.program.tys, span.clone());
                let callback = self.intern(TyKind::Fn(FnTy {
                    params: Box::new([element]),
                    ret: output,
                    effects: crate::EffectRow::Hidden,
                }));
                self.check_slot(body, call, 1, Some(callback), span.clone())?;
                Ok(self.wrap(container, self.resolved(output)))
            }
            RelOp::SortBy => {
                let key = self.infer.new_var(&mut self.program.tys, span.clone());
                let callback = self.intern(TyKind::Fn(FnTy {
                    params: Box::new([element]),
                    ret: key,
                    effects: crate::EffectRow::Hidden,
                }));
                self.check_slot(body, call, 1, Some(callback), span)?;
                Ok(self.wrap(container, element))
            }
            RelOp::Take | RelOp::Drop => {
                let int = self.intern(TyKind::Int);
                self.check_slot(body, call, 1, Some(int), span)?;
                Ok(self.wrap(container, element))
            }
            RelOp::Pairs => {
                let pair = self.intern(TyKind::Tuple(Box::new([element, element])));
                Ok(self.wrap(container, pair))
            }
            RelOp::Every | RelOp::Exists => {
                let bool_ty = self.intern(TyKind::Bool);
                if call.args.len() > 1 {
                    let callback = self.intern(TyKind::Fn(FnTy {
                        params: Box::new([element]),
                        ret: bool_ty,
                        effects: crate::EffectRow::Hidden,
                    }));
                    self.check_slot(body, call, 1, Some(callback), span)?;
                } else {
                    self.unify(element, bool_ty, span.clone());
                }
                Ok(bool_ty)
            }
            RelOp::AllUnique => {
                let key = self.infer.new_var(&mut self.program.tys, span.clone());
                let callback = self.intern(TyKind::Fn(FnTy {
                    params: Box::new([element]),
                    ret: key,
                    effects: crate::EffectRow::Hidden,
                }));
                self.check_slot(body, call, 1, Some(callback), span)?;
                Ok(self.intern(TyKind::Bool))
            }
            RelOp::SourceScan => Err(NotYetReason::Unsupported(
                "a source scan without a table declaration is not on HIR".into(),
            )),
            RelOp::BucketBy
            | RelOp::Window
            | RelOp::FlatMap
            | RelOp::Union
            | RelOp::Zip
            | RelOp::ZipExact
            | RelOp::AsOfJoin => Err(NotYetReason::Unsupported(
                format!("the relation operator `{:?}` is not on HIR yet", op).into(),
            )),
        }
    }

    fn container(&self, ty: Ty) -> Option<(Container, Ty)> {
        match self.kind_of(ty) {
            Some(TyKind::List(element)) => Some((Container::List, element)),
            Some(TyKind::Relation(element)) => Some((Container::Relation, element)),
            Some(TyKind::Stream(element)) => Some((Container::Stream, element)),
            _ => None,
        }
    }

    fn wrap(&mut self, container: Container, element: Ty) -> Ty {
        let kind = match container {
            Container::List => TyKind::List(element),
            Container::Relation => TyKind::Relation(element),
            Container::Stream => TyKind::Stream(element),
        };
        self.intern(kind)
    }

    fn argument_expr(&self, call: &crate::Call, slot: usize) -> Option<ExprId> {
        match call.args.get(slot)?.value {
            ArgValue::Expr(id) => Some(id),
            ArgValue::Default => None,
        }
    }

    /// Types one intrinsic call.
    fn intrinsic_ty(
        &mut self,
        body: &mut Body,
        call: &crate::Call,
        form: IntrinsicForm,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match form {
            IntrinsicForm::Fail => {
                self.check_slot(body, call, 0, None, span)?;
                Ok(self.intern(TyKind::Never))
            }
            IntrinsicForm::OptionSome => {
                let Some(value) = self.argument_expr(call, 0) else {
                    self.conflict("`Some` needs one value", span);
                    return Ok(self.error());
                };
                let value_ty = self.infer_expr(body, value, None)?;
                Ok(self.intern(TyKind::Option(value_ty)))
            }
            IntrinsicForm::Now => Ok(self.intern(TyKind::Instant)),
            IntrinsicForm::Uuid7 => Ok(self.intern(TyKind::Uuid)),
            IntrinsicForm::Error => Err(NotYetReason::Unsupported(
                "`error(...)` constructs the `core` `Error` value; the declaration is not on \
                 HIR until H2b/H5"
                    .into(),
            )),
            IntrinsicForm::Parallel
            | IntrinsicForm::Race
            | IntrinsicForm::Timeout
            | IntrinsicForm::MergeStreams => Err(NotYetReason::Unsupported(
                "concurrency intrinsics are not on HIR in H2a".into(),
            )),
        }
    }

    /// Types a constructor call.
    fn ctor_call_ty(
        &mut self,
        body: &mut Body,
        call: &crate::Call,
        def: DefId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        if super::quantity::is_unit(self.program, def) {
            return self.unit_ctor_ty(body, def, call, span);
        }
        let result = self.ctor_result_ty(def, span.clone())?;
        let fields = self.ctor_fields(def)?;
        for (index, field) in fields.iter().enumerate() {
            let field_ty = self.field_in(result, *field)?;
            if call.args.get(index).is_some() {
                if index == 0 && crate::engine_money::is_money_def(self.program, def) {
                    self.check_money_amount(body, def, call, field_ty, span.clone())?;
                } else {
                    self.check_slot(body, call, index, Some(field_ty), span.clone())?;
                }
            }
        }
        if call.args.len() > fields.len() {
            self.conflict(
                "the constructor supplies more fields than it declares",
                span,
            );
        }
        Ok(result)
    }

    /// Types the amount of `Money<C>(value)`. A `Float` amount is the `E5004` class (hir-host.md B5); any other
    /// amount is checked against the field as `check_slot` checks it.
    fn check_money_amount(
        &mut self,
        body: &mut Body,
        def: DefId,
        call: &crate::Call,
        field_ty: Ty,
        span: SyntaxSpan,
    ) -> Result<(), NotYetReason> {
        let Some(argument) = call.args.first() else {
            return Ok(());
        };
        let ArgValue::Expr(id) = argument.value else {
            return Ok(());
        };
        let ty = self.infer_expr(body, id, None)?;
        if let Some(code) = crate::engine_codes::construction_rejection(self.program, def, ty) {
            self.conflicts += 1;
            self.diagnostics.push(crate::HirDiagnostic::new(
                code,
                Box::<str>::from("a Money amount must be a Decimal, not a Float"),
                span,
            ));
            return Ok(());
        }
        self.coerce_at(body, id, ty, field_ty, span);
        Ok(())
    }

    /// Types a nominal or variant construction.
    pub(super) fn check_construct(
        &mut self,
        body: &mut Body,
        def: DefId,
        fields: &[(DefId, ExprId)],
        expected: Option<Ty>,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let result = self.ctor_result_ty(def, span.clone())?;
        let declared = self.ctor_fields(def)?;
        if let Some(expected) = expected {
            self.unify(result, expected, span.clone());
        }
        for (field, value) in fields {
            let field_ty = self.field_in(result, *field)?;
            self.infer_expr(body, *value, Some(field_ty))?;
        }
        let required = self.table_required_fields(def).unwrap_or_else(|| {
            declared
                .iter()
                .filter_map(|field| self.program.defs.get(*field).map(|def| def.name.clone()))
                .collect()
        });
        for field in required {
            let supplied = fields.iter().any(|(candidate, _)| {
                self.program
                    .defs
                    .get(*candidate)
                    .is_some_and(|definition| definition.name == field)
            });
            if !supplied {
                self.conflict(
                    format!("the constructor omits the field `{}`", field.as_str()),
                    span.clone(),
                );
            }
        }
        Ok(result)
    }

    /// The result type of a constructor: the enclosing enum for a variant, the
    /// declared nominal otherwise. A generic nominal's result carries one fresh
    /// variable per declared parameter, for the caller to solve.
    pub(super) fn ctor_result_ty(
        &mut self,
        def: DefId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match self
            .program
            .defs
            .get(def)
            .map(|definition| definition.kind.clone())
        {
            Some(DefKind::Variant(_)) => {
                let owner = self.variant_owner.get(&def).copied().ok_or_else(|| {
                    NotYetReason::Unsupported(
                        "an enum variant has no recorded enclosing enum".into(),
                    )
                })?;
                Ok(self.intern(TyKind::Adt(owner, Box::new([]))))
            }
            Some(DefKind::Type(_)) => {
                let args = self.fresh_nominal_args(def, &span);
                Ok(self.intern(TyKind::Adt(def, args)))
            }
            Some(DefKind::Enum(_) | DefKind::Table(_)) => {
                Ok(self.intern(TyKind::Adt(def, Box::new([]))))
            }
            Some(DefKind::Unit(_)) => Ok(self.intern(TyKind::Unit)),
            _ => Err(NotYetReason::Unsupported(
                "this definition is not a constructor".into(),
            )),
        }
    }

    /// The field definitions a constructor declares, in declaration order.
    fn ctor_fields(&self, def: DefId) -> Result<Vec<DefId>, NotYetReason> {
        let fields = match self
            .program
            .defs
            .get(def)
            .map(|definition| definition.kind.clone())
        {
            Some(DefKind::Variant(variant)) => variant.fields,
            Some(DefKind::Table(table)) => table.fields,
            Some(DefKind::Type(definition)) => definition
                .members
                .iter()
                .copied()
                .filter(|member| {
                    self.program
                        .defs
                        .get(*member)
                        .is_some_and(|definition| definition.kind.is_field())
                })
                .collect(),
            Some(DefKind::Enum(enumeration)) => enumeration.variants,
            _ => {
                return Err(NotYetReason::Unsupported(
                    "this definition is not a constructor".into(),
                ));
            }
        };
        Ok(fields)
    }

    /// The declared fields of a nominal type, by name.
    pub(super) fn nominal_fields(&self, def: DefId) -> Option<Vec<(Name, Ty)>> {
        let fields = self.ctor_fields(def).ok()?;
        Some(
            fields
                .iter()
                .filter_map(|field| {
                    let definition = self.program.defs.get(*field)?;
                    let DefKind::Field(field_definition) = &definition.kind else {
                        return None;
                    };
                    Some((definition.name.clone(), field_definition.ty))
                })
                .collect(),
        )
    }

    pub(super) fn field_definition_ty(&mut self, field: DefId) -> Result<Ty, NotYetReason> {
        match self
            .program
            .defs
            .get(field)
            .map(|definition| definition.kind.clone())
        {
            Some(DefKind::Field(definition)) => Ok(definition.ty),
            _ => Err(NotYetReason::Unsupported(
                "a field reference does not name a field definition".into(),
            )),
        }
    }

    /// Types a field access and settles the written member name.
    pub(super) fn field_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        base: ExprId,
        field: &FieldRef,
        _span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let base_ty = self.infer_expr(body, base, None)?;
        let resolved = self.resolved(base_ty);
        match field {
            FieldRef::Def(def) => self.field_in(resolved, *def),
            FieldRef::Index(index) => match self.kind_of(resolved) {
                Some(TyKind::Record(fields)) => fields
                    .get(*index as usize)
                    .map(|(_, ty)| *ty)
                    .ok_or_else(|| {
                        NotYetReason::Unsupported("a record field index is out of range".into())
                    }),
                Some(TyKind::Error) => Ok(self.error()),
                _ => Err(NotYetReason::Unsupported(
                    "an indexed field access on this type is not on HIR".into(),
                )),
            },
            FieldRef::Name(name) => match self.kind_of(resolved) {
                Some(TyKind::Adt(def, _)) => {
                    let Some(field_def) = self.find_field(def, name) else {
                        if matches!(self.kind_of(resolved), Some(TyKind::Error)) {
                            return Ok(self.error());
                        }
                        return Err(NotYetReason::Unsupported(
                            format!(
                                "the field `{}` is not declared by this nominal type on HIR",
                                name.as_str()
                            )
                            .into(),
                        ));
                    };
                    let field_ty = self.field_in(resolved, field_def)?;
                    body.exprs[id].kind = ExprKind::Field {
                        base,
                        field: FieldRef::Def(field_def),
                    };
                    Ok(field_ty)
                }
                Some(TyKind::Record(fields)) => {
                    let Some(index) = fields.iter().position(|(field, _)| field == name) else {
                        return Err(NotYetReason::Unsupported(
                            "the record has no field with this name on HIR".into(),
                        ));
                    };
                    body.exprs[id].kind = ExprKind::Field {
                        base,
                        field: FieldRef::Index(u32::try_from(index).expect("field index fits u32")),
                    };
                    Ok(fields[index].1)
                }
                Some(TyKind::Relation(inner)) => {
                    let Some(TyKind::Adt(table, _)) = self.kind_of(inner) else {
                        return Err(NotYetReason::Unsupported(
                            "a relation selector needs a declared table row".into(),
                        ));
                    };
                    let Some(field_def) = self.find_field(table, name) else {
                        return Err(NotYetReason::Unsupported(
                            "the table does not declare this field".into(),
                        ));
                    };
                    let field_ty = self.field_definition_ty(field_def)?;
                    body.exprs[id].kind = ExprKind::Field {
                        base,
                        field: FieldRef::Def(field_def),
                    };
                    let row = self.intern(TyKind::Adt(table, Box::new([])));
                    Ok(self.intern(TyKind::Fn(FnTy {
                        params: Box::new([row]),
                        ret: field_ty,
                        effects: crate::EffectRow::Hidden,
                    })))
                }
                Some(TyKind::Error) => Ok(self.error()),
                _ => Err(NotYetReason::Unsupported(
                    format!(
                        "the field `{}` on this value type is not on HIR",
                        name.as_str()
                    )
                    .into(),
                )),
            },
        }
    }

    /// The definition of `name` among a nominal's fields.
    fn find_field(&self, owner: DefId, name: &Name) -> Option<DefId> {
        let fields = self.ctor_fields(owner).ok()?;
        fields.into_iter().find(|field| {
            self.program
                .defs
                .get(*field)
                .is_some_and(|definition| definition.name == *name && definition.kind.is_field())
        })
    }

    /// The type of an assignment place.
    pub(super) fn place_ty(
        &mut self,
        body: &mut Body,
        place: &Place,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match place {
            Place::Local(local) => Ok(body.locals[*local].ty),
            Place::Field { base, field } => {
                let base_ty = self.place_ty(body, base, span.clone())?;
                let resolved = self.resolved(base_ty);
                match field {
                    FieldRef::Def(def) => self.field_in(base_ty, *def),
                    FieldRef::Index(index) => match self.kind_of(resolved) {
                        Some(TyKind::Record(fields)) => fields
                            .get(*index as usize)
                            .map(|(_, ty)| *ty)
                            .ok_or_else(|| {
                                NotYetReason::Unsupported(
                                    "a record field index is out of range".into(),
                                )
                            }),
                        _ => Err(NotYetReason::Unsupported(
                            "an indexed field assignment on this type is not on HIR".into(),
                        )),
                    },
                    FieldRef::Name(name) => match self.kind_of(resolved) {
                        Some(TyKind::Adt(def, _)) => {
                            let Some(field) = self.find_field(def, name) else {
                                return Err(NotYetReason::Unsupported(
                                    "assignment to an undeclared field is not on HIR".into(),
                                ));
                            };
                            self.field_in(resolved, field)
                        }
                        Some(TyKind::Record(fields)) => fields
                            .iter()
                            .find(|(field, _)| field == name)
                            .map(|(_, ty)| *ty)
                            .ok_or_else(|| {
                                NotYetReason::Unsupported(
                                    "assignment to an undeclared record field is not on HIR".into(),
                                )
                            }),
                        _ => Err(NotYetReason::Unsupported(
                            "assignment through this place is not on HIR".into(),
                        )),
                    },
                }
            }
            Place::Index { base, index } => {
                let base_ty = self.place_ty(body, base, span.clone())?;
                let index_ty = self.infer_expr(body, *index, None)?;
                let int = self.intern(TyKind::Int);
                self.unify(index_ty, int, span.clone());
                match self.kind_of(self.resolved(base_ty)) {
                    Some(TyKind::List(element)) => Ok(element),
                    _ => Err(NotYetReason::Unsupported(
                        "indexed assignment on this place is not on HIR".into(),
                    )),
                }
            }
        }
    }

    /// The value type of a definition reference.
    pub(super) fn def_value_ty(
        &mut self,
        def: DefId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match self
            .program
            .defs
            .get(def)
            .map(|definition| definition.kind.clone())
        {
            Some(DefKind::Fn(function)) => {
                if !function.generics.is_empty() {
                    return Err(NotYetReason::Unsupported(
                        "a generic function value needs explicit type arguments, stage H2b".into(),
                    ));
                }
                if function.params.iter().any(|parameter| {
                    matches!(self.program.tys.kind(parameter.ty), Some(TyKind::Error))
                }) {
                    return Err(NotYetReason::UnannotatedParameter(def));
                }
                if matches!(self.program.tys.kind(function.ret), Some(TyKind::Error)) {
                    return Err(NotYetReason::Unsupported(
                        "a function value whose result type is not on HIR yet".into(),
                    ));
                }
                let params = function
                    .params
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect::<Vec<_>>();
                Ok(self.intern(TyKind::Fn(FnTy {
                    params: params.into_boxed_slice(),
                    ret: function.ret,
                    effects: crate::EffectRow::Hidden,
                })))
            }
            Some(DefKind::Table(_)) => {
                let row = self.intern(TyKind::Adt(def, Box::new([])));
                Ok(self.intern(TyKind::Relation(row)))
            }
            Some(DefKind::Enum(_)) => Ok(self.intern(TyKind::Adt(def, Box::new([])))),
            Some(DefKind::Type(_)) if self.program.is_sys_relation(def) => {
                let row = self.intern(TyKind::Adt(def, Box::new([])));
                Ok(self.intern(TyKind::Relation(row)))
            }
            Some(DefKind::Type(_) | DefKind::NativeType(_)) => {
                let args = self.fresh_nominal_args(def, &span);
                Ok(self.intern(TyKind::Adt(def, args)))
            }
            Some(DefKind::NativeStatic(native)) => Ok(native.ty),
            Some(DefKind::Variant(variant)) => {
                let result = self.ctor_result_ty(def, span.clone())?;
                if variant.fields.is_empty() {
                    return Ok(result);
                }
                let mut params = Vec::with_capacity(variant.fields.len());
                for field in &variant.fields {
                    params.push(self.field_in(result, *field)?);
                }
                Ok(self.intern(TyKind::Fn(FnTy {
                    params: params.into_boxed_slice(),
                    ret: result,
                    effects: crate::EffectRow::Hidden,
                })))
            }
            Some(DefKind::Field(definition)) => Ok(definition.ty),
            Some(DefKind::Let(binding)) => {
                let body = &self.program.bodies[binding.body];
                let declared = binding
                    .declared
                    .filter(|ty| !matches!(self.program.tys.kind(*ty), Some(TyKind::Error)));
                let ty = declared.unwrap_or(body.exprs[body.root].ty);
                if matches!(self.program.tys.kind(ty), Some(TyKind::Error)) {
                    return Err(NotYetReason::Unsupported(
                        "the module-level binding's type is not on HIR yet".into(),
                    ));
                }
                Ok(ty)
            }
            Some(DefKind::Unit(_)) => Ok(self.intern(TyKind::Unit)),
            Some(
                DefKind::Assertion(_)
                | DefKind::Dimension(_)
                | DefKind::Impl(_)
                | DefKind::Protocol(_),
            ) => Err(NotYetReason::Unsupported(
                "this definition has no H2a value type".into(),
            )),
            None => Err(NotYetReason::Unsupported(
                "the definition is out of range".into(),
            )),
        }
    }

    /// Types a record literal against a nominal or structural record type.
    pub(super) fn check_record(
        &mut self,
        body: &mut Body,
        fields: &[(Name, ExprId)],
        expected: Ty,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let resolved = self.resolved(expected);
        let computed = self.table_computed_fields(resolved);
        let (declared, required) = match self.kind_of(resolved) {
            Some(TyKind::Adt(def, _)) => (
                self.nominal_fields_of(resolved).ok_or_else(|| {
                    NotYetReason::Unsupported(
                        "a record literal for this nominal type is not on HIR".into(),
                    )
                })?,
                self.table_required_fields(def),
            ),
            Some(TyKind::Record(declared)) => (
                declared
                    .iter()
                    .map(|(name, ty)| (name.clone(), *ty))
                    .collect::<Vec<_>>(),
                None,
            ),
            _ => {
                return Err(NotYetReason::Unsupported(
                    "a record literal against this type is not on HIR".into(),
                ));
            }
        };
        for (name, value) in fields {
            let Some((_, field_ty)) = declared.iter().find(|(field, _)| field == name) else {
                self.conflict(
                    format!("the record type has no field `{}`", name.as_str()),
                    span.clone(),
                );
                continue;
            };
            if computed.contains(name) {
                self.conflict(
                    format!(
                        "the field `{}` is computed from the row and is read-only",
                        name.as_str()
                    ),
                    span.clone(),
                );
            }
            self.infer_expr(body, *value, Some(*field_ty))?;
        }
        // A table row requires its defaultless fields (`table_required_fields`).
        // A nominal type's fields are its representation, so all of them are required.
        let required =
            required.unwrap_or_else(|| declared.iter().map(|(name, _)| name.clone()).collect());
        for name in required {
            if !fields.iter().any(|(field, _)| *field == name) {
                self.conflict(
                    format!("the record literal omits the field `{}`", name.as_str()),
                    span.clone(),
                );
            }
        }
        Ok(resolved)
    }
}

/// The arguments with `receiver` in slot 0 and each written argument moved one slot right.
fn with_receiver(arguments: &[Arg], receiver: ExprId) -> Box<[Arg]> {
    std::iter::once(Arg {
        slot: crate::ParamIdx::new(0),
        value: ArgValue::Expr(receiver),
    })
    .chain(arguments.iter().map(|argument| Arg {
        slot: crate::ParamIdx::new(argument.slot.as_u32() + 1),
        value: argument.value,
    }))
    .collect()
}
