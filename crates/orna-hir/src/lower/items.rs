//! Phase 3: signatures and bodies (design sections 2.2 to 2.7).
//!
//! Phase 3 runs in two passes. The first builds every definition's signature:
//! generic bounds, parameter types, defaults, member fields and variants. It
//! may allocate new nested definitions, so it iterates a growing list. The
//! second lowers every body, once the parts of the signature a body needs are
//! final.
//!
//! A default expression, a module-level `let` and an assertion are bodies of
//! their own, owned by the definition that declares them, exactly as a function
//! body is.

use orna_syntax::{
    EnumPayloadField, EnumVariant, Expr, FunctionSignature, GenericParameter, ImplMember,
    Implementation, NativeEffectSet, Parameter, Pattern, ProtocolMember, SyntaxSpan, TableMember,
    TypeExpr, TypeMember, TypeRepresentation,
};

use crate::{
    AssertionDef, BodyId, DefId, DefKind, DimensionDef, EnumDef, FieldDef, GenericParam, Idx,
    ImplDef, LetDef, ModuleId, ParamDef, ProtocolDef, Stmt, TableDef, TypeDef, UnitDef, VariantDef,
    Visibility,
};

use super::declare::{Builder, DeclAst, PendingFunction, PendingKind};
use super::exprs::lower_expr;
use super::{BodyState, pats};

/// One body to lower once signatures are final.
pub(crate) enum BodyJob {
    /// A function's own body.
    Function(DefId),
    /// A default, initializer or assertion expression.
    Expression {
        /// The definition that owns the body.
        def: DefId,
        /// The allocated body id.
        body: BodyId,
        /// The expression the body evaluates.
        expr: Box<Expr>,
        /// The owner whose row an assertion body writes as `self`.
        self_ty: Option<DefId>,
    },
}

impl Builder<'_> {
    /// Builds every signature, then lowers every body.
    pub(crate) fn lower_items(&mut self) {
        let mut index = 0;
        while index < self.pending.len() {
            let def = DefId::new(u32::try_from(index).expect("definition index fits u32"));
            let ast = match &mut self.pending[index].kind {
                PendingKind::Unbuilt(_) => {
                    match std::mem::replace(
                        &mut self.pending[index].kind,
                        PendingKind::Built(DefKind::Unit(UnitDef)),
                    ) {
                        PendingKind::Unbuilt(ast) => Some(ast),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(ast) = ast {
                self.build_declaration(def, *ast);
            }
            index += 1;
        }
        let jobs = std::mem::take(&mut self.body_jobs);
        for job in jobs {
            match job {
                BodyJob::Function(def) => self.lower_function_body(def),
                BodyJob::Expression {
                    def,
                    body,
                    expr,
                    self_ty,
                } => self.lower_expression_body(def, body, &expr, self_ty),
            }
        }
    }

    /// Lowers the generics, parameters and result of a function signature. Parameter
    /// defaults are queued as body jobs.
    fn build_function_head(
        &mut self,
        def: DefId,
        module: ModuleId,
        signature: &FunctionSignature,
    ) -> PendingFunction {
        let generics = self.build_generics(module, &signature.generics);
        let (params, defaults) = self.build_parameters(def, module, &signature.parameters, None);
        for (body, expr) in defaults {
            self.body_jobs.push(BodyJob::Expression {
                def,
                body,
                expr: Box::new(expr),
                self_ty: None,
            });
        }
        let ret = self.build_result(module, signature.result.as_ref());
        PendingFunction {
            generics,
            params,
            ret,
            effects: crate::EffectSet::empty(),
            body: None,
            body_expr: None,
            patterns: Vec::new(),
            native: None,
        }
    }

    /// Builds the kind of one top-level declaration.
    fn build_declaration(&mut self, def: DefId, ast: DeclAst) {
        let module = self.pending[def.index()].module;
        match ast {
            DeclAst::Function(signature, body) => {
                let body_id = self.allocate_body(def);
                let mut head = self.build_function_head(def, module, &signature);
                head.body = Some(body_id);
                head.body_expr = Some(body);
                head.patterns = signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.pattern.clone())
                    .collect();
                self.pending[def.index()].kind = PendingKind::Function(Box::new(head));
                self.body_jobs.push(BodyJob::Function(def));
            }
            DeclAst::Native(signature, clause) => {
                let mut head = self.build_function_head(def, module, &signature);
                let key = crate::NativeKey {
                    module: self.program.modules[module].path.clone(),
                    name: crate::Name::new(&signature.name),
                };
                head.native = Some(crate::NativeDecl {
                    abi: self.program.native_abi,
                    key,
                    effects: declared_effects(clause.as_ref()),
                });
                self.pending[def.index()].kind = PendingKind::Function(Box::new(head));
            }
            DeclAst::Table(keys, members) => self.build_table(def, module, &keys, &members),
            DeclAst::Protocol(generics, members) => {
                self.build_protocol(def, module, &generics, &members);
            }
            DeclAst::Enum(generics, variants) => {
                self.build_enum(def, module, &generics, &variants);
            }
            DeclAst::Dimension => {
                self.pending[def.index()].kind =
                    PendingKind::Built(DefKind::Dimension(DimensionDef { unit: None }));
            }
            DeclAst::Unit(dimension, definition) => {
                let ty = super::types::lower_type(self, module, &[], &dimension);
                let _ = ty;
                self.pending[def.index()].kind = PendingKind::Built(DefKind::Unit(UnitDef));
                if let orna_syntax::UnitDefinition::Derived { value, offset, .. } = definition {
                    // A derived unit's expression is not part of the value
                    // model, but its names are still written in the module:
                    // lower it so the editor index sees them.
                    let expressions = std::iter::once(value.clone())
                        .chain(offset.clone())
                        .collect::<Vec<_>>();
                    for expr in expressions {
                        let body = self.allocate_body(def);
                        self.body_jobs.push(BodyJob::Expression {
                            def,
                            body,
                            expr: Box::new(expr),
                            self_ty: None,
                        });
                    }
                }
            }
            DeclAst::Type(generics, representation) => {
                self.build_type(def, module, &generics, &representation);
            }
            DeclAst::Assertion(value) => {
                let body = self.allocate_body(def);
                self.pending[def.index()].kind =
                    PendingKind::Built(DefKind::Assertion(AssertionDef { body }));
                self.body_jobs.push(BodyJob::Expression {
                    def,
                    body,
                    expr: Box::new(value),
                    self_ty: None,
                });
            }
            DeclAst::Let(annotation, value) => {
                let body = self.allocate_body(def);
                if let Some(annotation) = annotation {
                    let _ = super::types::lower_type(self, module, &[], &annotation);
                }
                self.pending[def.index()].kind = PendingKind::Built(DefKind::Let(LetDef { body }));
                self.body_jobs.push(BodyJob::Expression {
                    def,
                    body,
                    expr: Box::new(value),
                    self_ty: None,
                });
            }
        }
    }

    /// Builds a table's fields, keys, assertions and implementations.
    fn build_table(
        &mut self,
        def: DefId,
        module: ModuleId,
        keys: &[Parameter],
        members: &[TableMember],
    ) {
        let mut fields = Vec::new();
        let mut key = Vec::new();
        for (index, parameter) in keys.iter().enumerate() {
            let ty = match parameter.annotation.as_ref() {
                Some(annotation) => super::types::lower_type(self, module, &[], annotation),
                None => self.program.tys.error(),
            };
            let field = self.declare_nested(
                module,
                &parameter_name(&parameter.pattern, index),
                Visibility::Public,
                parameter.span.clone(),
                DefKind::Field(FieldDef { ty }),
            );
            fields.push(field);
            key.push(field);
        }
        let mut assertions = Vec::new();
        for member in members {
            match member {
                TableMember::Field { name, ty, span, .. } => {
                    fields.push(self.build_field(module, name, ty, span.clone()));
                }
                TableMember::Assertion { value, span } => {
                    assertions.push(self.build_assertion(module, def, value, span.clone()));
                }
                TableMember::Implementation {
                    implementation,
                    span,
                } => {
                    self.build_implementation(def, module, implementation, span.clone());
                }
            }
        }
        self.pending[def.index()].kind = PendingKind::Built(DefKind::Table(TableDef {
            fields,
            key,
            assertions,
        }));
    }

    /// Builds one field definition.
    fn build_field(
        &mut self,
        module: ModuleId,
        name: &str,
        ty: &TypeExpr,
        span: SyntaxSpan,
    ) -> DefId {
        let ty = super::types::lower_type(self, module, &[], ty);
        self.declare_nested(
            module,
            name,
            Visibility::Public,
            span,
            DefKind::Field(FieldDef { ty }),
        )
    }

    /// Builds one assertion definition and its body job.
    fn build_assertion(
        &mut self,
        module: ModuleId,
        owner: DefId,
        value: &Expr,
        span: SyntaxSpan,
    ) -> DefId {
        let body = self.allocate_body(owner);
        let def = self.declare_nested(
            module,
            "assert",
            Visibility::Private,
            span,
            DefKind::Assertion(AssertionDef { body }),
        );
        self.body_jobs.push(BodyJob::Expression {
            def,
            body,
            expr: Box::new(value.clone()),
            self_ty: Some(owner),
        });
        def
    }

    /// Builds a protocol: member signatures, with no bodies.
    fn build_protocol(
        &mut self,
        def: DefId,
        module: ModuleId,
        _generics: &[GenericParameter],
        members: &[ProtocolMember],
    ) {
        let mut lowered = Vec::new();
        for member in members {
            match member {
                ProtocolMember::Function { signature, span } => {
                    let member_def = self.declare_nested(
                        module,
                        &signature.name,
                        Visibility::Public,
                        span.clone(),
                        DefKind::Unit(UnitDef),
                    );
                    let kind = self.signature_only(module, signature);
                    self.pending[member_def.index()].kind = PendingKind::Built(kind);
                    lowered.push(member_def);
                }
                ProtocolMember::Static { name, ty, span } => {
                    let ty = super::types::lower_type(self, module, &[], ty);
                    lowered.push(self.declare_nested(
                        module,
                        name,
                        Visibility::Public,
                        span.clone(),
                        DefKind::Field(FieldDef { ty }),
                    ));
                }
            }
        }
        self.pending[def.index()].kind =
            PendingKind::Built(DefKind::Protocol(ProtocolDef { members: lowered }));
    }

    /// Builds an enum and its variants.
    fn build_enum(
        &mut self,
        def: DefId,
        module: ModuleId,
        _generics: &[GenericParameter],
        variants: &[EnumVariant],
    ) {
        let mut lowered = Vec::new();
        for variant in variants {
            let fields = variant
                .fields
                .iter()
                .map(|field| self.build_payload_field(module, field))
                .collect::<Vec<_>>();
            lowered.push(self.declare_nested(
                module,
                &variant.name,
                Visibility::Public,
                variant.span.clone(),
                DefKind::Variant(VariantDef { fields }),
            ));
        }
        self.pending[def.index()].kind =
            PendingKind::Built(DefKind::Enum(EnumDef { variants: lowered }));
    }

    /// Builds one enum payload field.
    fn build_payload_field(&mut self, module: ModuleId, field: &EnumPayloadField) -> DefId {
        let ty = super::types::lower_type(self, module, &[], &field.ty);
        self.declare_nested(
            module,
            &field.name,
            Visibility::Public,
            field.span.clone(),
            DefKind::Field(FieldDef { ty }),
        )
    }

    /// Builds a nominal type or alias and its members.
    fn build_type(
        &mut self,
        def: DefId,
        module: ModuleId,
        generics: &[GenericParameter],
        representation: &TypeRepresentation,
    ) {
        let params = self.build_generics(module, generics);
        let mut members = Vec::new();
        let type_members = match representation {
            TypeRepresentation::Alias { ty, refinements } => {
                let _ = super::types::lower_type(self, module, &[], ty);
                refinements.as_slice()
            }
            TypeRepresentation::Nominal { members } => members.as_slice(),
        };
        for member in type_members {
            match member {
                TypeMember::Field {
                    name,
                    ty,
                    visibility,
                    span,
                    ..
                } => {
                    let vis = if *visibility {
                        Visibility::Public
                    } else {
                        Visibility::Private
                    };
                    let ty = super::types::lower_type(self, module, &[], ty);
                    members.push(self.declare_nested(
                        module,
                        name,
                        vis,
                        span.clone(),
                        DefKind::Field(FieldDef { ty }),
                    ));
                }
                TypeMember::Assertion { value, span } => {
                    members.push(self.build_assertion(module, def, value, span.clone()));
                }
                TypeMember::Implementation {
                    implementation,
                    span,
                } => {
                    self.build_implementation(def, module, implementation, span.clone());
                }
            }
        }
        self.pending[def.index()].kind =
            PendingKind::Built(DefKind::Type(TypeDef { params, members }));
    }

    /// Builds one protocol implementation.
    fn build_implementation(
        &mut self,
        owner: DefId,
        module: ModuleId,
        implementation: &Implementation,
        span: SyntaxSpan,
    ) -> DefId {
        let protocol = self.resolve_protocol(module, &implementation.protocol);
        // A method's receiver `self` has the implementing type (decision D1: the receiver is not written out).
        let receiver = self
            .program
            .tys
            .intern(crate::TyKind::Adt(owner, Box::new([])));
        let mut members = Vec::new();
        for member in &implementation.members {
            match member {
                ImplMember::Function {
                    signature,
                    body,
                    span,
                } => {
                    let member_def = self.declare_nested(
                        module,
                        &signature.name,
                        Visibility::Public,
                        span.clone(),
                        DefKind::Unit(UnitDef),
                    );
                    let body_id = self.allocate_body(member_def);
                    let generics = self.build_generics(module, &signature.generics);
                    let (params, defaults) = self.build_parameters(
                        member_def,
                        module,
                        &signature.parameters,
                        Some(receiver),
                    );
                    for (default_body, expr) in defaults {
                        self.body_jobs.push(BodyJob::Expression {
                            def: member_def,
                            body: default_body,
                            expr: Box::new(expr),
                            self_ty: None,
                        });
                    }
                    let ret = self.build_result(module, signature.result.as_ref());
                    let patterns = signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.pattern.clone())
                        .collect();
                    self.pending[member_def.index()].kind =
                        PendingKind::Function(Box::new(PendingFunction {
                            generics,
                            params,
                            ret,
                            effects: crate::EffectSet::empty(),
                            body: Some(body_id),
                            body_expr: Some(body.clone()),
                            patterns,
                            native: None,
                        }));
                    self.body_jobs.push(BodyJob::Function(member_def));
                    members.push(member_def);
                }
                ImplMember::Static {
                    name,
                    ty,
                    value,
                    span,
                } => {
                    let member_def = self.declare_nested(
                        module,
                        name,
                        Visibility::Public,
                        span.clone(),
                        DefKind::Unit(UnitDef),
                    );
                    let body = self.allocate_body(member_def);
                    if let Some(ty) = ty {
                        let _ = super::types::lower_type(self, module, &[], ty);
                    }
                    self.pending[member_def.index()].kind =
                        PendingKind::Built(DefKind::Let(LetDef { body }));
                    self.body_jobs.push(BodyJob::Expression {
                        def: member_def,
                        body,
                        expr: Box::new(value.clone()),
                        self_ty: None,
                    });
                    members.push(member_def);
                }
            }
        }
        self.declare_nested(
            module,
            "impl",
            Visibility::Private,
            span,
            DefKind::Impl(ImplDef { protocol, members }),
        )
    }

    /// Resolves the protocol an implementation names, recording the use.
    fn resolve_protocol(&mut self, module: ModuleId, protocol: &TypeExpr) -> Option<DefId> {
        match protocol {
            TypeExpr::Name { path, span, .. } if !path.is_empty() => {
                let segments = path
                    .iter()
                    .map(|segment| crate::Name::new(segment))
                    .collect::<Vec<_>>();
                let last = segments
                    .last()
                    .cloned()
                    .unwrap_or_else(|| crate::Name::new(""));
                let name_span = orna_syntax::SyntaxSpan::new(
                    span.end.saturating_sub(last.as_str().len()),
                    span.end,
                );
                match self.resolve_path(module, &segments) {
                    super::super::resolve::PathKind::Def(def) => {
                        super::record_use(
                            &mut self.program,
                            module,
                            name_span,
                            last,
                            crate::UseContext::Type,
                            crate::NameTarget::Def(def),
                        );
                        Some(def)
                    }
                    _ => {
                        super::record_use(
                            &mut self.program,
                            module,
                            name_span,
                            last,
                            crate::UseContext::Type,
                            crate::NameTarget::Unresolved,
                        );
                        None
                    }
                }
            }
            _ => None,
        }
    }

    /// The generic parameters of a declaration, with resolved protocol bounds.
    fn build_generics(
        &mut self,
        module: ModuleId,
        generics: &[GenericParameter],
    ) -> Vec<GenericParam> {
        generics
            .iter()
            .map(|generic| {
                let bounds = generic
                    .bounds
                    .iter()
                    .filter_map(|bound| match bound {
                        TypeExpr::Name { path, .. } if !path.is_empty() => {
                            let segments = path
                                .iter()
                                .map(|segment| crate::Name::new(segment))
                                .collect::<Vec<_>>();
                            match self.resolve_path(module, &segments) {
                                super::super::resolve::PathKind::Def(def) => Some(def),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .collect();
                GenericParam {
                    name: crate::Name::new(&generic.name),
                    bounds,
                }
            })
            .collect()
    }

    /// The parameters of a function, with default bodies.
    fn build_parameters(
        &mut self,
        def: DefId,
        module: ModuleId,
        parameters: &[Parameter],
        receiver: Option<crate::Ty>,
    ) -> (Vec<ParamDef>, Vec<(BodyId, Expr)>) {
        let mut built = Vec::with_capacity(parameters.len());
        let mut defaults = Vec::new();
        for (index, parameter) in parameters.iter().enumerate() {
            let name = parameter_name(&parameter.pattern, index);
            let is_receiver = index == 0 && name == "self";
            let (ty, written) = match (&parameter.annotation, receiver) {
                (Some(annotation), _) => (
                    super::types::lower_type(self, module, &[], annotation),
                    true,
                ),
                (None, Some(receiver)) if is_receiver => (receiver, true),
                (None, _) => (self.program.tys.error(), false),
            };
            let default = parameter.default.as_ref().map(|expr| {
                let body = self.allocate_body(def);
                defaults.push((body, expr.clone()));
                body
            });
            built.push(ParamDef {
                name: crate::Name::new(&name),
                ty,
                written,
                default,
                binder: None,
            });
        }
        (built, defaults)
    }

    /// The declared result type.
    fn build_result(&mut self, module: ModuleId, result: Option<&TypeExpr>) -> crate::Ty {
        match result {
            Some(ty) => super::types::lower_type(self, module, &[], ty),
            None => self.program.tys.error(),
        }
    }

    /// A signature with no body, for a protocol member.
    fn signature_only(&mut self, module: ModuleId, signature: &FunctionSignature) -> DefKind {
        let generics = self.build_generics(module, &signature.generics);
        let ret = self.build_result(module, signature.result.as_ref());
        let params = signature
            .parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let written = parameter.annotation.is_some();
                let ty = match &parameter.annotation {
                    Some(annotation) => super::types::lower_type(self, module, &[], annotation),
                    None => self.program.tys.error(),
                };
                ParamDef {
                    name: crate::Name::new(&parameter_name(&parameter.pattern, index)),
                    ty,
                    written,
                    default: None,
                    binder: None,
                }
            })
            .collect();
        DefKind::Fn(crate::FnDef {
            generics,
            params,
            ret,
            effects: crate::EffectSet::empty(),
            body: crate::FnBody::Signature,
        })
    }

    /// Lowers one function body: parameter slots, binder patterns, then the
    /// expression.
    fn lower_function_body(&mut self, def: DefId) {
        let (module, params, patterns, body_id, body_expr) = {
            let pending = &self.pending[def.index()];
            let PendingKind::Function(function) = &pending.kind else {
                return;
            };
            let Some(body_id) = function.body else {
                return;
            };
            let Some(body_expr) = function.body_expr.clone() else {
                return;
            };
            (
                pending.module,
                function.params.clone(),
                function.patterns.clone(),
                body_id,
                body_expr,
            )
        };
        let mut state = BodyState::new(body_id, def, module);
        state.push_scope(body_expr.span().start);
        let mut prologue = Vec::new();
        for (index, (param, pattern)) in params.iter().zip(patterns.iter()).enumerate() {
            let span = pattern.span().clone();
            let local = state.add_local(param.name.clone(), param.ty, span.clone());
            state.params.push(local);
            if matches!(pattern, Pattern::Name(..)) {
                super::record_binding_use(
                    &mut self.program,
                    module,
                    state.id,
                    span,
                    param.name.clone(),
                    local,
                    false,
                );
            }
            if !matches!(pattern, Pattern::Name(..)) {
                let init = super::exprs::push(
                    self,
                    &mut state,
                    crate::ExprKind::Local(local),
                    pattern.span().clone(),
                );
                let pat = pats::lower_pattern_with(self, &mut state, pattern, Some(param.ty));
                prologue.push(Stmt::Let { pat, init });
            }
            let _ = index;
        }
        let root = lower_expr(self, &mut state, &body_expr);
        state.root = Some(if prologue.is_empty() {
            root
        } else {
            super::exprs::push(
                self,
                &mut state,
                crate::ExprKind::Block {
                    stmts: prologue.into_boxed_slice(),
                    tail: Some(root),
                },
                body_expr.span(),
            )
        });
        state.pop_scope(body_expr.span().end);
        let error_ty = self.program.tys.error();
        self.complete_body(state, error_ty);
    }

    /// Lowers one standalone expression body.
    fn lower_expression_body(
        &mut self,
        def: DefId,
        body_id: BodyId,
        expr: &Expr,
        self_ty: Option<DefId>,
    ) {
        let module = self.pending[def.index()].module;
        // An assertion body is owned by its table or type row, not by the assertion itself.
        let mut state = BodyState::new(body_id, self_ty.unwrap_or(def), module);
        state.push_scope(expr.span().start);
        if let Some(owner) = self_ty {
            // An assertion's owner row is written as `self` (design section
            // 2.4); the slot is the body's only parameter.
            let ty = self
                .program
                .tys
                .intern(crate::TyKind::Adt(owner, Box::new([])));
            let span = orna_syntax::SyntaxSpan::new(0, 0);
            let slot = state.add_local(crate::Name::new("self"), ty, span);
            state.params.push(slot);
        }
        let root = lower_expr(self, &mut state, expr);
        state.root = Some(root);
        state.pop_scope(expr.span().end);
        let error_ty = self.program.tys.error();
        self.complete_body(state, error_ty);
    }
}

/// The name a parameter pattern declares.
fn parameter_name(pattern: &Pattern, index: usize) -> String {
    match pattern {
        Pattern::Name(name, _) => name.clone(),
        _ => format!("_arg{index}"),
    }
}

/// The effect kinds a native declaration's `effect` clause names (ORNA-NATIVE-009).
/// A recovered parse with no clause contributes nothing; a class spelling outside
/// the vocabulary fails closed as `Opaque`.
fn declared_effects(clause: Option<&NativeEffectSet>) -> crate::EffectSet {
    match clause {
        Some(NativeEffectSet::Classes { classes, .. }) => {
            classes
                .iter()
                .fold(crate::EffectSet::empty(), |set, class| {
                    set.join(match crate::EffectKind::from_class(&class.text) {
                        Some(kind) => crate::EffectSet::single(kind),
                        None => crate::EffectSet::opaque(),
                    })
                })
        }
        Some(NativeEffectSet::None { .. }) | None => crate::EffectSet::empty(),
    }
}
