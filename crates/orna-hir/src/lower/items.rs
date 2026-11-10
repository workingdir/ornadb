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
        let names = generic_names(&signature.generics);
        let generics = self.build_generics(module, &names, &signature.generics);
        let (params, defaults) =
            self.build_parameters(def, module, &signature.parameters, None, &names);
        for (body, expr) in defaults {
            self.body_jobs.push(BodyJob::Expression {
                def,
                body,
                expr: Box::new(expr),
                self_ty: None,
            });
        }
        let ret = self.build_result(module, &names, signature.result.as_ref());
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
                let path = self.program.modules[module].path.clone();
                let name = crate::Name::new(&signature.name);
                let binding = match self.program.modules[module].origin {
                    crate::ModuleOrigin::Core | crate::ModuleOrigin::Sys => {
                        crate::NativeBinding::Engine {
                            abi: self.program.native_abi,
                        }
                    }
                    _ => {
                        let (symbol, ascii) = native_symbol(&path, &name);
                        if !ascii {
                            let span = self.pending[def.index()].span.clone();
                            self.diagnose(
                                module,
                                crate::diag::UNSUPPORTED,
                                format!("native `{}` has a name outside [A-Za-z0-9_]: no library symbol", signature.name),
                                span,
                            );
                        }
                        crate::NativeBinding::Library { symbol }
                    }
                };
                head.native = Some(crate::NativeDecl {
                    binding,
                    key: crate::NativeKey { module: path, name },
                    effects: declared_effects(clause.as_ref()),
                });
                self.pending[def.index()].kind = PendingKind::Function(Box::new(head));
            }
            DeclAst::Table(keys, members) => self.build_table(def, module, &keys, &members),
            DeclAst::Protocol(generics, members) => {
                self.build_protocol(def, module, &generics, &members);
            }
            DeclAst::Enum(generics, variants, implementations) => {
                self.build_enum(def, module, &generics, &variants, &implementations);
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
                    PendingKind::Built(DefKind::Assertion(AssertionDef { body, owner: def }));
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
            DefKind::Assertion(AssertionDef { body, owner }),
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
        generics: &[GenericParameter],
        members: &[ProtocolMember],
    ) {
        let mut names = generic_names(generics);
        names.push(crate::Name::new("Self"));
        let params = self.build_generics(module, &names, generics);
        // Each member is generic over the protocol's parameters, then `Self`, then its own.
        let mut inherited = params.clone();
        let self_args = (0..generics.len())
            .map(|index| {
                let index = u32::try_from(index).expect("generic parameters fit u32");
                self.program
                    .tys
                    .intern(crate::TyKind::Param(crate::ParamIdx::new(index)))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        inherited.push(GenericParam {
            name: crate::Name::new("Self"),
            bounds: vec![crate::Bound {
                protocol: def,
                args: self_args,
            }],
        });
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
                    let kind = self.signature_only(module, signature, &inherited);
                    self.pending[member_def.index()].kind = PendingKind::Built(kind);
                    lowered.push(member_def);
                }
                ProtocolMember::Static { name, ty, span } => {
                    let ty = super::types::lower_type(self, module, &names, ty);
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
        self.pending[def.index()].kind = PendingKind::Built(DefKind::Protocol(ProtocolDef {
            params,
            members: lowered,
        }));
    }

    /// Builds an enum, its variants and its nested implementations.
    fn build_enum(
        &mut self,
        def: DefId,
        module: ModuleId,
        generics: &[GenericParameter],
        variants: &[EnumVariant],
        implementations: &[Implementation],
    ) {
        let names = generic_names(generics);
        let mut lowered = Vec::new();
        for variant in variants {
            let fields = variant
                .fields
                .iter()
                .map(|field| self.build_payload_field(module, &names, field))
                .collect::<Vec<_>>();
            lowered.push(self.declare_nested(
                module,
                &variant.name,
                Visibility::Public,
                variant.span.clone(),
                DefKind::Variant(VariantDef { fields }),
            ));
        }
        let mut members = Vec::new();
        for implementation in implementations {
            let span = implementation.span.clone();
            members.push(self.build_implementation(def, module, implementation, span));
        }
        self.pending[def.index()].kind = PendingKind::Built(DefKind::Enum(EnumDef {
            variants: lowered,
            members,
        }));
    }

    /// Builds one enum payload field.
    fn build_payload_field(
        &mut self,
        module: ModuleId,
        names: &[crate::Name],
        field: &EnumPayloadField,
    ) -> DefId {
        let ty = super::types::lower_type(self, module, names, &field.ty);
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
        let names = generic_names(generics);
        let params = self.build_generics(module, &names, generics);
        let mut members = Vec::new();
        let type_members = match representation {
            TypeRepresentation::Alias { ty, refinements } => {
                let _ = super::types::lower_type(self, module, &names, ty);
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
                    let ty = super::types::lower_type(self, module, &names, ty);
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
                    let implementation =
                        self.build_implementation(def, module, implementation, span.clone());
                    members.push(implementation);
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
        // A method's receiver `self` has the implementing type (decision D1: the receiver is not written out),
        // and `Self` written in the implementation names that type.
        let receiver = self
            .program
            .tys
            .intern(crate::TyKind::Adt(owner, Box::new([])));
        self.self_ty = Some(receiver);
        let protocol = self.resolve_protocol(module, &implementation.protocol);
        let protocol_args = match &implementation.protocol {
            TypeExpr::Name { arguments, .. } => arguments
                .iter()
                .map(|argument| super::types::lower_type(self, module, &[], argument))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            _ => Vec::new().into_boxed_slice(),
        };
        let mut members = Vec::new();
        for member in &implementation.members {
            match member {
                ImplMember::Function {
                    signature,
                    body,
                    span,
                } => {
                    members.push(self.build_impl_function(module, receiver, signature, body, span))
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
        self.self_ty = None;
        let implementation_def = self.declare_nested(
            module,
            "impl",
            Visibility::Private,
            span,
            DefKind::Impl(ImplDef {
                owner,
                protocol,
                protocol_args,
                members,
            }),
        );
        if protocol.is_some() {
            self.program
                .instances
                .entry(owner)
                .or_default()
                .push(implementation_def);
        }
        implementation_def
    }

    /// Builds one function member of a protocol implementation.
    fn build_impl_function(
        &mut self,
        module: ModuleId,
        receiver: crate::Ty,
        signature: &FunctionSignature,
        body: &Expr,
        span: &SyntaxSpan,
    ) -> DefId {
        let member_def = self.declare_nested(
            module,
            &signature.name,
            Visibility::Public,
            span.clone(),
            DefKind::Unit(UnitDef),
        );
        let body_id = self.allocate_body(member_def);
        let names = generic_names(&signature.generics);
        let generics = self.build_generics(module, &names, &signature.generics);
        let (params, defaults) = self.build_parameters(
            member_def,
            module,
            &signature.parameters,
            Some(receiver),
            &names,
        );
        for (default_body, expr) in defaults {
            self.body_jobs.push(BodyJob::Expression {
                def: member_def,
                body: default_body,
                expr: Box::new(expr),
                self_ty: None,
            });
        }
        let ret = self.build_result(module, &names, signature.result.as_ref());
        let patterns = signature
            .parameters
            .iter()
            .map(|parameter| parameter.pattern.clone())
            .collect();
        self.pending[member_def.index()].kind = PendingKind::Function(Box::new(PendingFunction {
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
        member_def
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
    ///
    /// `names` is every type name in scope for the bounds' arguments: the
    /// enclosing parameters first, then `generics`.
    fn build_generics(
        &mut self,
        module: ModuleId,
        names: &[crate::Name],
        generics: &[GenericParameter],
    ) -> Vec<GenericParam> {
        generics
            .iter()
            .map(|generic| {
                let bounds = generic
                    .bounds
                    .iter()
                    .filter_map(|bound| self.build_bound(module, names, bound))
                    .collect();
                GenericParam {
                    name: crate::Name::new(&generic.name),
                    bounds,
                }
            })
            .collect()
    }

    /// Resolves one protocol bound and lowers its type arguments.
    fn build_bound(
        &mut self,
        module: ModuleId,
        names: &[crate::Name],
        bound: &TypeExpr,
    ) -> Option<crate::Bound> {
        let TypeExpr::Name {
            path, arguments, ..
        } = bound
        else {
            return None;
        };
        if path.is_empty() {
            return None;
        }
        let segments = path
            .iter()
            .map(|segment| crate::Name::new(segment))
            .collect::<Vec<_>>();
        let super::super::resolve::PathKind::Def(protocol) = self.resolve_path(module, &segments)
        else {
            return None;
        };
        let args = arguments
            .iter()
            .map(|argument| super::types::lower_type(self, module, names, argument))
            .collect::<Vec<_>>();
        Some(crate::Bound {
            protocol,
            args: args.into_boxed_slice(),
        })
    }

    /// The parameters of a function, with default bodies.
    fn build_parameters(
        &mut self,
        def: DefId,
        module: ModuleId,
        parameters: &[Parameter],
        receiver: Option<crate::Ty>,
        names: &[crate::Name],
    ) -> (Vec<ParamDef>, Vec<(BodyId, Expr)>) {
        let mut built = Vec::with_capacity(parameters.len());
        let mut defaults = Vec::new();
        for (index, parameter) in parameters.iter().enumerate() {
            let name = parameter_name(&parameter.pattern, index);
            let is_receiver = index == 0 && name == "self";
            let (ty, written) = match (&parameter.annotation, receiver) {
                (Some(annotation), _) => (
                    super::types::lower_type(self, module, names, annotation),
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
    fn build_result(
        &mut self,
        module: ModuleId,
        names: &[crate::Name],
        result: Option<&TypeExpr>,
    ) -> crate::Ty {
        match result {
            Some(ty) => super::types::lower_type(self, module, names, ty),
            None => self.program.tys.error(),
        }
    }

    /// A signature with no body, for a protocol member. Each member is generic over the protocol's parameters,
    /// then `Self` (bounded by the protocol, ORNA-GENERIC-002/003), then its own generics.
    fn signature_only(
        &mut self,
        module: ModuleId,
        signature: &FunctionSignature,
        inherited: &[GenericParam],
    ) -> DefKind {
        let mut names = inherited
            .iter()
            .map(|generic| generic.name.clone())
            .collect::<Vec<_>>();
        names.extend(generic_names(&signature.generics));
        let mut generics = inherited.to_vec();
        generics.extend(self.build_generics(module, &names, &signature.generics));
        let ret = self.build_result(module, &names, signature.result.as_ref());
        // The protocol's `Self` parameter types an unannotated receiver `self`, as in an implementation (D1).
        let self_ty = names
            .iter()
            .position(|name| *name == crate::Name::new("Self"))
            .map(|index| {
                let index = u32::try_from(index).expect("generic parameters fit u32");
                self.program
                    .tys
                    .intern(crate::TyKind::Param(crate::ParamIdx::new(index)))
            });
        let params = signature
            .parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let name = parameter_name(&parameter.pattern, index);
                let is_receiver = index == 0 && name == "self";
                let written = parameter.annotation.is_some() || is_receiver;
                let ty = match (&parameter.annotation, self_ty) {
                    (Some(annotation), _) => {
                        super::types::lower_type(self, module, &names, annotation)
                    }
                    (None, Some(self_ty)) if is_receiver => self_ty,
                    (None, _) => self.program.tys.error(),
                };
                ParamDef {
                    name: crate::Name::new(&name),
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
        let mut state = BodyState::new(body_id, def, module);
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

/// The declared generic names, in declaration order. A written type variable lowers to the parameter
/// at its position in this list.
fn generic_names(generics: &[GenericParameter]) -> Vec<crate::Name> {
    generics
        .iter()
        .map(|generic| crate::Name::new(&generic.name))
        .collect()
}

/// The C symbol a shared library exports for a native key (library document
/// section 4): `orna_` followed by the module path segments and the name joined
/// by `__`. The flag is false when a part is outside `[A-Za-z0-9_]`.
fn native_symbol(module: &crate::ModulePath, name: &crate::Name) -> (String, bool) {
    let parts: Vec<&str> = module
        .segments()
        .iter()
        .chain(std::iter::once(name))
        .map(crate::Name::as_str)
        .collect();
    let ascii = parts.iter().all(|part| {
        part.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    });
    (format!("orna_{}", parts.join("__")), ascii)
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
