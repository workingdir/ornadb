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

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use orna_syntax_ast::{
    EnumPayloadField, EnumVariant, Expr, FieldInitializer as SyntaxFieldInitializer,
    FunctionSignature, GenericParameter, ImplMember, Implementation, NativeEffectSet, Parameter,
    Pattern, ProtocolMember, TableMember, TypeExpr, TypeMember, TypeRepresentation,
};

use crate::{
    AssertionDef, BodyId, DefId, DefKind, DimensionDef, EnumDef, FieldDef, FieldInitializer,
    GenericParam, Idx, ImplDef, LetDef, ModuleId, NativeStaticDef, NativeTypeDef, ProtocolDef,
    Stmt, TableDef, Ty, TyKind, TypeDef, UnitDef, VariantDef, Visibility,
};

use super::declare::{Builder, DeclAst, PendingFunction, PendingKind};
use super::exprs::lower_expr;
use super::natives::{declared_effects, native_symbol};
use super::{BodyState, pats};

/// One body to lower once signatures are final.
#[derive(Clone)]
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
        /// The type of the subject a body writes as `self`: a table or nominal row, or a
        /// refined type's base value.
        self_ty: Option<Ty>,
    },
}

impl Builder {
    /// Builds every signature, then lowers every body.
    pub(crate) fn lower_items(&mut self) {
        let mut index = 0;
        while index < self.pending.len() {
            let def = DefId::new(u32::try_from(index).expect("definition index fits u32"));
            let ast = match &mut self.pending[index].kind {
                PendingKind::Unbuilt(_) => {
                    match std::mem::replace(
                        &mut self.pending[index].kind,
                        PendingKind::Built(DefKind::Unit(UnitDef::default())),
                    ) {
                        PendingKind::Unbuilt(ast) => Some(ast),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(ast) = ast {
                self.lowered += 1;
                self.build_declaration(def, *ast);
            }
            index += 1;
        }
        self.resolve_unit_jobs();
        let jobs = std::mem::take(&mut self.body_jobs);
        for job in jobs {
            self.lowered += 1;
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
            DeclAst::Enum(generics, variants, implementations, unit_family) => {
                self.build_enum(
                    def,
                    module,
                    &generics,
                    &variants,
                    &implementations,
                    unit_family,
                );
            }
            DeclAst::Dimension => {
                self.pending[def.index()].kind =
                    PendingKind::Built(DefKind::Dimension(DimensionDef { unit: None }));
            }
            DeclAst::Unit(dimension, definition) => {
                self.build_unit(def, module, dimension, definition)
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
                let declared = annotation
                    .as_ref()
                    .map(|annotation| super::types::lower_type(self, module, &[], annotation));
                self.pending[def.index()].kind =
                    PendingKind::Built(DefKind::Let(LetDef { body, declared }));
                self.body_jobs.push(BodyJob::Expression {
                    def,
                    body,
                    expr: Box::new(value),
                    self_ty: None,
                });
            }
            DeclAst::NativeType(generics) => self.build_native_type(def, module, &generics),
            DeclAst::NativeStatic(ty, clause) => self.build_native_static(def, module, ty, clause),
        }
    }

    /// Builds a native type's generic parameters. Its representation is host-owned.
    fn build_native_type(&mut self, def: DefId, module: ModuleId, generics: &[GenericParameter]) {
        let names = generic_names(generics);
        let params = self.build_generics(module, &names, generics);
        self.pending[def.index()].kind =
            PendingKind::Built(DefKind::NativeType(NativeTypeDef { params }));
    }

    /// Builds a native static from its declared type and effect clause.
    fn build_native_static(
        &mut self,
        def: DefId,
        module: ModuleId,
        annotation: Option<TypeExpr>,
        clause: Option<NativeEffectSet>,
    ) {
        let ty = match &annotation {
            Some(annotation) => super::types::lower_type(self, module, &[], annotation),
            None => self.program.tys.intern(TyKind::Error),
        };
        self.pending[def.index()].kind =
            PendingKind::Built(DefKind::NativeStatic(NativeStaticDef {
                ty,
                effects: declared_effects(clause.as_ref()),
            }));
    }

    /// Builds a unit: lowers its dimension, rejects the affine clauses and lowers
    /// the names a derived unit's value writes.
    fn build_unit(
        &mut self,
        def: DefId,
        module: ModuleId,
        dimension: TypeExpr,
        definition: orna_syntax_ast::UnitDefinition,
    ) {
        let _ = super::types::lower_type(self, module, &[], &dimension);
        let affine_clause = match &definition {
            orna_syntax_ast::UnitDefinition::Derived { offset, affine, .. } => {
                offset.is_some() || *affine
            }
            orna_syntax_ast::UnitDefinition::Base => false,
        };
        // Affine and offset clauses are library unit forms (units.md 8.5). The engine-embedded interface modules
        // declare them (core.units); a user or project module still gets S022.
        let embedded = matches!(
            self.program.modules[module].origin,
            crate::ModuleOrigin::Core | crate::ModuleOrigin::Sys
        );
        if affine_clause && !embedded {
            let span = self.pending[def.index()].span.clone();
            self.diagnose(
                module,
                crate::diag::UNSUPPORTED,
                "affine units are library types (units.md 8.5): `offset` and `affine` are not kernel clauses",
                span,
            );
        }
        let (offset, affine) = match &definition {
            orna_syntax_ast::UnitDefinition::Derived { offset, affine, .. } => {
                (super::units::literal_offset(offset.as_ref()), *affine)
            }
            orna_syntax_ast::UnitDefinition::Base => (None, false),
        };
        self.pending[def.index()].kind = PendingKind::Built(DefKind::Unit(UnitDef {
            family: None,
            factors: Vec::new(),
            offset,
            affine,
        }));
        if let orna_syntax_ast::UnitDefinition::Derived { value, offset, .. } = definition {
            // A derived unit's expression is not part of the value model, but its
            // names are still written in the module: lower it so the editor index
            // sees them.
            for expr in std::iter::once(value).chain(offset) {
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
                DefKind::Field(FieldDef {
                    ty,
                    initializer: None,
                }),
            );
            fields.push(field);
            key.push(field);
            if let Some(value) = &parameter.default {
                let initializer = SyntaxFieldInitializer::Default(value.clone());
                self.attach_initializer(field, ty, Some(def), &initializer);
            }
        }
        let mut assertions = Vec::new();
        for member in members {
            match member {
                TableMember::Field {
                    name,
                    ty,
                    initializer,
                    span,
                } => {
                    fields.push(self.build_field(
                        def,
                        module,
                        name,
                        ty,
                        initializer.as_ref(),
                        span.clone(),
                    ));
                }
                TableMember::Assertion { value, span } => {
                    let row = self.row_type(def);
                    assertions.push(self.build_assertion(module, def, value, span.clone(), row));
                }
                TableMember::Implementation {
                    implementation,
                    span,
                } => {
                    self.build_implementation(def, module, &[], implementation, span.clone());
                }
            }
        }
        self.pending[def.index()].kind = PendingKind::Built(DefKind::Table(TableDef {
            fields,
            key,
            assertions,
        }));
    }

    /// Builds one table field. An initializer lowers to a body owned by the field, with `self`
    /// naming the row, as an assertion's body is.
    fn build_field(
        &mut self,
        table: DefId,
        module: ModuleId,
        name: &str,
        ty: &TypeExpr,
        initializer: Option<&SyntaxFieldInitializer>,
        span: SyntaxSpan,
    ) -> DefId {
        let ty = super::types::lower_type(self, module, &[], ty);
        let field = self.declare_nested(
            module,
            name,
            Visibility::Public,
            span,
            DefKind::Field(FieldDef {
                ty,
                initializer: None,
            }),
        );
        if let Some(initializer) = initializer {
            self.attach_initializer(field, ty, Some(table), initializer);
        }
        field
    }

    /// Attaches an initializer body to a field that already exists. The field owns the body. An `owner`
    /// names the row a table field's body reads as `self`; a nominal default has none and is closed.
    fn attach_initializer(
        &mut self,
        field: DefId,
        ty: Ty,
        owner: Option<DefId>,
        initializer: &SyntaxFieldInitializer,
    ) {
        let (kind, value) = self.plan_initializer(field, initializer);
        self.pending[field.index()].kind = PendingKind::Built(DefKind::Field(FieldDef {
            ty,
            initializer: Some(kind),
        }));
        let self_ty = owner.map(|table| self.row_type(table));
        self.body_jobs.push(BodyJob::Expression {
            def: field,
            body: kind.body(),
            expr: Box::new(value),
            self_ty,
        });
    }

    /// Allocates the body an initializer lowers into. The field owns it, as an assertion owns its body.
    fn plan_initializer(
        &mut self,
        field: DefId,
        initializer: &SyntaxFieldInitializer,
    ) -> (FieldInitializer, Expr) {
        let body = self.allocate_body(field);
        match initializer {
            SyntaxFieldInitializer::Default(value) => {
                (FieldInitializer::Default(body), value.clone())
            }
            SyntaxFieldInitializer::Computed(value) => {
                (FieldInitializer::Computed(body), value.clone())
            }
        }
    }

    /// The type of a table's or nominal type's row, which an assertion body reads as `self`.
    fn row_type(&mut self, owner: DefId) -> Ty {
        self.program.tys.intern(TyKind::Adt(owner, Box::new([])))
    }

    /// Builds one assertion definition and its body job. `subject` is the type `self` names in
    /// the body: the row for a table or nominal type, the base value for a refined type.
    fn build_assertion(
        &mut self,
        module: ModuleId,
        owner: DefId,
        value: &Expr,
        span: SyntaxSpan,
        subject: Ty,
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
            self_ty: Some(subject),
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
                        DefKind::Unit(UnitDef::default()),
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
                        DefKind::Field(FieldDef {
                            ty,
                            initializer: None,
                        }),
                    ));
                }
            }
        }
        self.pending[def.index()].kind = PendingKind::Built(DefKind::Protocol(ProtocolDef {
            params,
            members: lowered,
        }));
    }

    /// Builds an enum, its variants and its nested implementations. In a unit family
    /// (`impl Unit {}`) each variant is a unit, and a derived variant's expression is
    /// queued for resolution once every definition is built.
    fn build_enum(
        &mut self,
        def: DefId,
        module: ModuleId,
        generics: &[GenericParameter],
        variants: &[EnumVariant],
        implementations: &[Implementation],
        unit_family: bool,
    ) {
        let names = generic_names(generics);
        let inherited = self.build_generics(module, &names, generics);
        let mut lowered = Vec::new();
        for variant in variants {
            if unit_family {
                lowered.push(self.build_family_unit(module, def, variant));
                continue;
            }
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
        let dimension = unit_family.then(|| super::units::family_dimension(variants, &lowered));
        let mut members = Vec::new();
        for implementation in implementations {
            let span = implementation.span.clone();
            members.push(self.build_implementation(def, module, &inherited, implementation, span));
        }
        self.pending[def.index()].kind = PendingKind::Built(DefKind::Enum(EnumDef {
            variants: lowered,
            members,
            dimension,
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
            DefKind::Field(FieldDef {
                ty,
                initializer: None,
            }),
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
        let (type_members, refined_base) = match representation {
            TypeRepresentation::Alias { ty, refinements } => (
                refinements.as_slice(),
                Some(super::types::lower_type(self, module, &names, ty)),
            ),
            TypeRepresentation::Nominal { members } => (members.as_slice(), None),
        };
        for member in type_members {
            match member {
                TypeMember::Field {
                    name,
                    ty,
                    visibility,
                    initializer,
                    span,
                } => {
                    let vis = if *visibility {
                        Visibility::Public
                    } else {
                        Visibility::Private
                    };
                    let ty = super::types::lower_type(self, module, &names, ty);
                    let field = self.declare_nested(
                        module,
                        name,
                        vis,
                        span.clone(),
                        DefKind::Field(FieldDef {
                            ty,
                            initializer: None,
                        }),
                    );
                    if let Some(value) = initializer {
                        let default = SyntaxFieldInitializer::Default(value.clone());
                        self.attach_initializer(field, ty, None, &default);
                    }
                    members.push(field);
                }
                TypeMember::Assertion { value, span } => {
                    let assertion = match refined_base {
                        Some(base) => self.build_assertion(
                            module,
                            def,
                            &implicit_subject(value),
                            span.clone(),
                            base,
                        ),
                        None => {
                            let row = self.row_type(def);
                            self.build_assertion(module, def, value, span.clone(), row)
                        }
                    };
                    members.push(assertion);
                }
                TypeMember::Implementation {
                    implementation,
                    span,
                } => {
                    let implementation = self.build_implementation(
                        def,
                        module,
                        &params,
                        implementation,
                        span.clone(),
                    );
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
        inherited: &[GenericParam],
        implementation: &Implementation,
        span: SyntaxSpan,
    ) -> DefId {
        // A method's receiver `self` has the implementing type (decision D1: the receiver is not written out),
        // and `Self` written in the implementation names that type.
        let names = inherited
            .iter()
            .map(|generic| generic.name.clone())
            .collect::<Vec<_>>();
        let args = (0..inherited.len())
            .map(|index| {
                let index = u32::try_from(index).expect("generic parameters fit u32");
                self.program
                    .tys
                    .intern(crate::TyKind::Param(crate::ParamIdx::new(index)))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let receiver = self.program.tys.intern(crate::TyKind::Adt(owner, args));
        self.self_ty = Some(receiver);
        let protocol = self.resolve_protocol(module, &implementation.protocol);
        let protocol_args = match &implementation.protocol {
            TypeExpr::Name { arguments, .. } => arguments
                .iter()
                .map(|argument| super::types::lower_type(self, module, &names, argument))
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
                } => members.push(
                    self.build_impl_function(module, receiver, inherited, signature, body, span),
                ),
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
                        DefKind::Unit(UnitDef::default()),
                    );
                    let body = self.allocate_body(member_def);
                    let declared = ty
                        .as_ref()
                        .map(|ty| super::types::lower_type(self, module, &names, ty));
                    self.pending[member_def.index()].kind =
                        PendingKind::Built(DefKind::Let(LetDef { body, declared }));
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
        inherited: &[GenericParam],
        signature: &FunctionSignature,
        body: &Expr,
        span: &SyntaxSpan,
    ) -> DefId {
        let member_def = self.declare_nested(
            module,
            &signature.name,
            Visibility::Public,
            span.clone(),
            DefKind::Unit(UnitDef::default()),
        );
        let body_id = self.allocate_body(member_def);
        let mut names = inherited
            .iter()
            .map(|generic| generic.name.clone())
            .collect::<Vec<_>>();
        names.extend(generic_names(&signature.generics));
        let mut generics = inherited.to_vec();
        generics.extend(self.build_generics(module, &names, &signature.generics));
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
                let name_span = orna_syntax_ast::SourceSpan::new(
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
        self_ty: Option<Ty>,
    ) {
        let module = self.pending[def.index()].module;
        let mut state = BodyState::new(body_id, def, module);
        state.push_scope(expr.span().start);
        if let Some(ty) = self_ty {
            // An assertion's subject is written as `self` (design section 2.4); the slot
            // is the body's only parameter.
            let span = orna_syntax_ast::SourceSpan::new(0, 0);
            let slot = state.add_local(crate::Name::new("self"), ty, span);
            if let (
                super::declare::PendingKind::Built(crate::DefKind::Field(_)),
                Some(&crate::TyKind::Adt(table, _)),
            ) = (&self.pending[def.index()].kind, self.program.tys.kind(ty))
            {
                state.row = Some(super::row::RowScope { slot, table });
            }
            state.params.push(slot);
        }
        let root = lower_expr(self, &mut state, expr);
        state.root = Some(root);
        state.pop_scope(expr.span().end);
        let error_ty = self.program.tys.error();
        self.complete_body(state, error_ty);
    }
}

/// A refined type's predicate may omit its subject: `assert >= 0` compares the value being
/// refined, which the body names `self`. Written as `self >= 0`, it lowers like any comparison.
fn implicit_subject(value: &Expr) -> Expr {
    match value {
        Expr::Unary { op, rhs, span }
            if matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=" | "in") =>
        {
            Expr::Binary {
                lhs: Box::new(Expr::Name {
                    text: "self".to_owned(),
                    span: span.clone(),
                }),
                op: op.clone(),
                rhs: rhs.clone(),
                span: span.clone(),
            }
        }
        _ => value.clone(),
    }
}

/// The name a parameter pattern declares.
pub(super) fn parameter_name(pattern: &Pattern, index: usize) -> String {
    match pattern {
        Pattern::Name(name, _) => name.clone(),
        _ => format!("_arg{index}"),
    }
}

/// The declared generic names, in declaration order. A written type variable lowers to the parameter
/// at its position in this list.
pub(super) fn generic_names(generics: &[GenericParameter]) -> Vec<crate::Name> {
    generics
        .iter()
        .map(|generic| crate::Name::new(&generic.name))
        .collect()
}
