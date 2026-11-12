//! Phase 1: modules, definitions and their ids (design H1).
//!
//! Every declaration becomes one [`crate::Def`] with a stable [`DefId`] before
//! any body is lowered, because bodies resolve names against those ids. The
//! module graph is registered here too: one [`crate::Module`] per source, the
//! built-in `sys` module, and the implicit `std` root.
//!
//! [`PendingDef`] keeps the syntax the declaration came from so phase 3 can
//! build its `DefKind` once types and bodies exist. Nothing outside this
//! module sees a pending definition.

use std::collections::BTreeMap;

use orna_syntax::{
    Declaration, EnumVariant, Expr, FunctionSignature, GenericParameter, Implementation,
    InterfaceClauses, Item, NativeEffectSet, Parameter, Pattern, ProtocolMember, SyntaxSpan,
    TableMember, TypeExpr, TypeRepresentation, UnitDefinition, UseTail,
};

use crate::Idx;
use crate::resolve::ModuleScope;
use crate::{
    Def, DefId, DefKind, EffectSet, FnBody, FnDef, IndexVec, Module, ModuleId, ModuleOrigin,
    ModulePath, Name, Program, SourceId, Visibility,
};

use super::{BodyState, SourceUnit};

/// The synthetic source id the built-in modules' spans refer to.
pub(crate) const BUILTIN_SOURCE: u32 = u32::MAX;

/// One `use` declaration awaiting resolution.
pub(crate) struct PendingUse {
    /// The importing module.
    pub(crate) module: ModuleId,
    /// The written path.
    pub(crate) path: Vec<orna_syntax::ImportSegment>,
    /// The written tail.
    pub(crate) tail: UseTail,
    /// The declaration's span.
    pub(crate) span: SyntaxSpan,
}

/// One declaration, before its kind is built.
pub(crate) struct PendingDef {
    /// The declaring module.
    pub(crate) module: ModuleId,
    /// The declared name.
    pub(crate) name: Name,
    /// The declared visibility.
    pub(crate) vis: Visibility,
    /// Where it was written.
    pub(crate) span: SyntaxSpan,
    /// The `///` doc comment lines above the declaration.
    pub(crate) docs: Vec<String>,
    /// The interface clauses of the declaration.
    pub(crate) clauses: InterfaceClauses,
    /// What it declares.
    pub(crate) kind: PendingKind,
}

/// What a unit suffix resolves to: no unit of that name, the one unit, or several that are ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnitLookup {
    None,
    Found(DefId),
    Ambiguous(Vec<DefId>),
}

/// The state of one definition while lowering.
pub(crate) enum PendingKind {
    /// A top-level declaration whose `DefKind` needs types or bodies.
    Unbuilt(Box<DeclAst>),
    /// A function, with an Orna body when it has one. A protocol member has no
    /// body and finishes as [`FnBody::Signature`].
    Function(Box<PendingFunction>),
    /// A definition whose kind is already complete.
    Built(DefKind),
}

/// A function while its signature and body are being lowered.
pub(crate) struct PendingFunction {
    /// Generic parameters, with protocol bounds resolved in phase 3.
    pub(crate) generics: Vec<crate::GenericParam>,
    /// Parameter definitions, in order, with their types and defaults.
    pub(crate) params: Vec<crate::ParamDef>,
    /// The result type.
    pub(crate) ret: crate::Ty,
    /// The effects; empty until stage H3 infers them.
    pub(crate) effects: EffectSet,
    /// The body, when this is an Orna function.
    pub(crate) body: Option<crate::BodyId>,
    /// The body expression, until phase 3 lowers it.
    pub(crate) body_expr: Option<Expr>,
    /// The declared parameter patterns, in parameter order, for binder slots.
    pub(crate) patterns: Vec<Pattern>,
    /// The native declaration, for a body-less `native fn` in an embedded module.
    pub(crate) native: Option<crate::NativeDecl>,
}

/// The declaration syntax a pending definition retains.
pub(crate) enum DeclAst {
    /// A function.
    Function(FunctionSignature, Expr),
    /// A body-less native function, in any module unit (ORNA-NATIVE-004).
    Native(FunctionSignature, Option<NativeEffectSet>),
    /// A host-supplied opaque nominal type (`native type`).
    NativeType(Vec<GenericParameter>),
    /// A host-supplied singleton value (`native static`) with its written type and declared effects.
    NativeStatic(Option<TypeExpr>, Option<NativeEffectSet>),
    /// A table.
    Table(Vec<Parameter>, Vec<TableMember>),
    /// A protocol.
    Protocol(Vec<GenericParameter>, Vec<ProtocolMember>),
    /// An enum, with its variants, nested implementations and whether it is a unit family.
    Enum(
        Vec<GenericParameter>,
        Vec<EnumVariant>,
        Vec<Implementation>,
        bool,
    ),
    /// A dimension.
    Dimension,
    /// A unit.
    Unit(TypeExpr, UnitDefinition),
    /// A type.
    Type(Vec<GenericParameter>, TypeRepresentation),
    /// A module or table assertion.
    Assertion(Expr),
    /// A module-level binding.
    Let(Option<TypeExpr>, Expr),
}

impl DeclAst {
    /// Copies the payload out of a parsed declaration.
    fn of(declaration: &Declaration) -> Self {
        match declaration {
            Declaration::Function { signature, body } => {
                Self::Function(signature.clone(), body.clone())
            }
            Declaration::Table { keys, members, .. } => Self::Table(keys.clone(), members.clone()),
            Declaration::Protocol {
                generics, members, ..
            } => Self::Protocol(generics.clone(), members.clone()),
            Declaration::Enum {
                generics,
                variants,
                implementations,
                unit_family,
                ..
            } => Self::Enum(
                generics.clone(),
                variants.clone(),
                implementations.clone(),
                *unit_family,
            ),
            Declaration::Dimension { .. } => Self::Dimension,
            Declaration::Unit {
                dimension,
                definition,
                ..
            } => Self::Unit(dimension.clone(), definition.clone()),
            Declaration::Type {
                generics,
                representation,
                ..
            } => Self::Type(generics.clone(), representation.clone()),
            Declaration::Assertion { value } => Self::Assertion(value.clone()),
            Declaration::Let {
                annotation, value, ..
            } => Self::Let(annotation.clone(), value.clone()),
            Declaration::Native { signature, effects } => {
                Self::Native(signature.clone(), effects.clone())
            }
            Declaration::NativeType { generics, .. } => Self::NativeType(generics.clone()),
            Declaration::NativeStatic {
                annotation,
                effects,
                ..
            } => Self::NativeStatic(annotation.clone(), effects.clone()),
            // `declare_item` diverts `use` declarations before this mapping runs.
            Declaration::Use { .. } => Self::Dimension,
        }
    }
}

/// Phase 1 through 4 state.
pub(crate) struct Builder<'a> {
    /// The sources being lowered.
    pub(crate) sources: &'a [SourceUnit<'a>],
    /// The program under construction.
    pub(crate) program: Program,
    /// Definitions by id.
    pub(crate) pending: Vec<PendingDef>,
    /// Module scopes, indexed by `ModuleId`.
    pub(crate) scopes: Vec<ModuleScope>,
    /// Canonical module paths to module ids.
    pub(crate) by_path: BTreeMap<ModulePath, ModuleId>,
    /// Unresolved `use` declarations, in source order.
    pub(crate) uses: Vec<PendingUse>,
    /// Completed bodies, indexed by `BodyId`.
    pub(crate) bodies: Vec<Option<crate::Body>>,
    /// Owner recorded for each allocated body id.
    pub(crate) body_owners: Vec<DefId>,
    /// The built-in `sys` module.
    pub(crate) sys_module: ModuleId,
    /// The implicit `std` root, when the program holds standard modules.
    pub(crate) std_module: Option<ModuleId>,
    /// Bodies to lower once every signature is final.
    pub(crate) body_jobs: Vec<super::items::BodyJob>,
    /// Unit-family variant expressions, resolved once every definition is built.
    pub(crate) unit_jobs: Vec<super::units::UnitJob>,
    /// The type `Self` names while an implementation's members are lowered.
    pub(crate) self_ty: Option<crate::Ty>,
    /// The names each module exports through `use module as _;` (ORNA-IMPORT-005).
    pub(crate) prelude: super::prelude::PreludeExports,
}

impl<'a> Builder<'a> {
    /// Creates the program skeleton and registers every source module.
    pub(crate) fn new(
        sources: &'a [SourceUnit<'a>],
        prelude: &super::prelude::PreludeExports,
    ) -> Self {
        let mut builder = Self {
            prelude: prelude.clone(),
            sources,
            program: Program::empty(),
            pending: Vec::new(),
            scopes: Vec::new(),
            by_path: BTreeMap::new(),
            uses: Vec::new(),
            bodies: Vec::new(),
            body_owners: Vec::new(),
            sys_module: ModuleId::new(0),
            std_module: None,
            body_jobs: Vec::new(),
            unit_jobs: Vec::new(),
            self_ty: None,
        };
        let mut registered = Vec::with_capacity(sources.len());
        for unit in sources {
            let origin = origin_of(&unit.path);
            let module = builder.add_module(unit.path.clone(), origin, unit.source);
            registered.push(module);
        }
        // `lower` always prepends the embedded interfaces, which include `sys` (D12).
        builder.sys_module = builder.by_path[&ModulePath::new([Name::new("sys")])];
        builder.program.native_abi = crate::ENGINE_NATIVE_ABI;
        builder.register_implicit_std();
        let roots = super::imports::implicit_roots(builder.std_module, builder.sys_module);
        for scope in &mut builder.scopes {
            for (name, module) in &roots {
                scope.modules.insert(name.clone(), *module);
            }
        }
        builder.declare_sources(&registered);
        builder
    }

    /// Adds one module and registers its path.
    fn add_module(&mut self, path: ModulePath, origin: ModuleOrigin, source: SourceId) -> ModuleId {
        let module = self.program.modules.push(Module {
            path,
            origin,
            source,
            items: Vec::new(),
            exports: BTreeMap::new(),
            imports: Vec::new(),
            diagnostics: Vec::new(),
            name_uses: Vec::new(),
        });
        self.scopes.push(ModuleScope::default());
        let path = self.program.modules[module].path.clone();
        if self.by_path.insert(path, module).is_some() {
            self.diagnose(
                module,
                crate::diag::IMPORT,
                "two modules define the same path",
                SyntaxSpan::new(0, 0),
            );
        }
        module
    }

    /// Registers an implicit `std` root when the program holds standard
    /// modules: the checker binds it in every scope (ORNA-IMPORT-001).
    fn register_implicit_std(&mut self) {
        let has_std = self.program.modules.iter().any(|module| {
            module
                .path
                .segments()
                .first()
                .is_some_and(|name| name.as_str() == "std")
        });
        if !has_std {
            return;
        }
        let root = ModulePath::new([Name::new("std")]);
        let id = match self.by_path.get(&root) {
            Some(id) => *id,
            None => self.add_module(root, ModuleOrigin::Std, SourceId::new(BUILTIN_SOURCE)),
        };
        self.std_module = Some(id);
    }

    /// Declares every top-level item of every source module.
    fn declare_sources(&mut self, modules: &[ModuleId]) {
        for (unit, module) in self.sources.iter().zip(modules) {
            for item in &unit.tree.items {
                self.declare_item(*module, item);
            }
        }
    }

    /// Declares one top-level item.
    fn declare_item(&mut self, module: ModuleId, item: &Item) {
        if let Declaration::Use { path, tail } = &item.declaration {
            self.uses.push(PendingUse {
                module,
                path: path.clone(),
                tail: tail.clone(),
                span: item.span.clone(),
            });
            return;
        }
        let Some(name) = item_name(&item.declaration) else {
            self.diagnose(
                module,
                crate::diag::UNSUPPORTED,
                "declaration has no name",
                item.span.clone(),
            );
            return;
        };
        let vis = visibility(&item.visibility);
        let decl = DeclAst::of(&item.declaration);
        let def = self.declare_top_level(module, name, vis, item.span.clone(), decl);
        self.pending[def.index()].docs = item.docs.clone();
        self.pending[def.index()].clauses = item.clauses.clone();
        if vis == Visibility::Public {
            let exported = self.pending[def.index()].name.clone();
            let inserted = self.program.modules[module]
                .exports
                .insert(exported.clone(), def)
                .is_none_or(|existing| existing == def);
            if !inserted {
                self.diagnose(
                    module,
                    crate::diag::DUPLICATE,
                    format!("two definitions are named `{exported}`"),
                    item.span.clone(),
                );
            }
        }
    }

    /// Allocates one top-level definition and binds its name in the module
    /// scope.
    pub(crate) fn declare_top_level(
        &mut self,
        module: ModuleId,
        name: &str,
        vis: Visibility,
        span: SyntaxSpan,
        decl: DeclAst,
    ) -> DefId {
        let name = Name::new(name);
        let def = self.allocate(
            module,
            name.clone(),
            vis,
            span.clone(),
            PendingKind::Unbuilt(Box::new(decl)),
        );
        let occupied = self.scopes[module.index()]
            .values
            .insert(name.clone(), def)
            .is_some_and(|existing| existing != def);
        self.scopes[module.index()].local_names.insert(name.clone());
        if occupied {
            self.diagnose(
                module,
                crate::diag::DUPLICATE,
                format!("`{name}` is declared twice in this module"),
                span,
            );
        }
        def
    }

    /// Allocates one nested definition (a field, variant or member) that is
    /// part of its owner and not exported.
    pub(crate) fn declare_nested(
        &mut self,
        module: ModuleId,
        name: &str,
        vis: Visibility,
        span: SyntaxSpan,
        kind: DefKind,
    ) -> DefId {
        self.allocate(module, Name::new(name), vis, span, PendingKind::Built(kind))
    }

    fn allocate(
        &mut self,
        module: ModuleId,
        name: Name,
        vis: Visibility,
        span: SyntaxSpan,
        kind: PendingKind,
    ) -> DefId {
        let def = DefId::new(
            u32::try_from(self.pending.len()).expect("HIR holds at most u32::MAX definitions"),
        );
        self.pending.push(PendingDef {
            module,
            name,
            vis,
            span: span.clone(),
            docs: Vec::new(),
            clauses: InterfaceClauses::default(),
            kind,
        });
        self.program.modules[module].items.push(def);
        def
    }

    /// Allocates a body id owned by `owner`.
    pub(crate) fn allocate_body(&mut self, owner: DefId) -> crate::BodyId {
        let id = crate::BodyId::new(
            u32::try_from(self.bodies.len()).expect("HIR holds at most u32::MAX bodies"),
        );
        self.bodies.push(None);
        self.body_owners.push(owner);
        id
    }

    /// Stores a finished body under its allocated id.
    pub(crate) fn complete_body(&mut self, state: BodyState, error_ty: crate::Ty) {
        let id = state.id;
        let body = state.finish(EffectSet::empty(), error_ty);
        if let Some(slot) = self.bodies.get_mut(id.index()) {
            *slot = Some(body);
        }
    }

    /// Records one diagnostic on a module.
    pub(crate) fn diagnose(
        &mut self,
        module: ModuleId,
        code: &'static str,
        message: impl Into<Box<str>>,
        span: SyntaxSpan,
    ) {
        self.program.modules[module]
            .diagnostics
            .push(crate::HirDiagnostic::new(code, message, span));
    }

    /// The definition's name.
    pub(crate) fn def_name(&self, def: DefId) -> &Name {
        &self.pending[def.index()].name
    }

    /// The unit declarations a unit suffix may name: those of this module first, otherwise those of the whole
    /// program. One candidate is the unit; several are ambiguous, and the caller reports them.
    pub(crate) fn resolve_unit(&self, module: ModuleId, name: &Name) -> UnitLookup {
        let candidates = self
            .pending
            .iter()
            .enumerate()
            .filter_map(|(index, pending)| {
                matches!(pending.kind, PendingKind::Built(crate::DefKind::Unit(_)))
                    .then_some(DefId::new(u32::try_from(index).ok()?))
            })
            .filter(|def| &self.pending[def.index()].name == name)
            .collect::<Vec<_>>();
        let own = candidates
            .iter()
            .copied()
            .filter(|def| self.pending[def.index()].module == module)
            .collect::<Vec<_>>();
        let scoped = if own.is_empty() { candidates } else { own };
        match scoped.as_slice() {
            [] => UnitLookup::None,
            [unit] => UnitLookup::Found(*unit),
            _ => UnitLookup::Ambiguous(scoped),
        }
    }

    /// Whether `def` is a unit declaration.
    pub(crate) fn is_unit(&self, def: DefId) -> bool {
        matches!(
            self.pending.get(def.index()).map(|pending| &pending.kind),
            Some(PendingKind::Built(crate::DefKind::Unit(_)))
        )
    }

    /// The member of `base` named `name`: a variant, field or nested member. A static member of one of the
    /// base's implementations is a member too, so `Shape.name` reads the constant `static name` declares.
    pub(crate) fn member_def(&self, base: DefId, name: &Name) -> Option<DefId> {
        let (members, implementations) = match &self.pending[base.index()].kind {
            PendingKind::Built(crate::DefKind::Enum(enumeration)) => {
                (enumeration.variants.clone(), enumeration.members.clone())
            }
            PendingKind::Built(crate::DefKind::Type(definition)) => {
                (definition.members.clone(), definition.members.clone())
            }
            PendingKind::Built(crate::DefKind::Table(table)) => (table.fields.clone(), Vec::new()),
            PendingKind::Built(crate::DefKind::Variant(variant)) => {
                (variant.fields.clone(), Vec::new())
            }
            _ => (Vec::new(), Vec::new()),
        };
        let direct = members
            .into_iter()
            .find(|member| &self.pending[member.index()].name == name);
        direct.or_else(|| self.implementation_static(&implementations, name))
    }

    /// The static constant named `name` in one of the implementations `implementations` holds.
    fn implementation_static(&self, implementations: &[DefId], name: &Name) -> Option<DefId> {
        implementations.iter().find_map(|implementation| {
            let PendingKind::Built(crate::DefKind::Impl(implementation)) =
                &self.pending[implementation.index()].kind
            else {
                return None;
            };
            implementation.members.iter().copied().find(|member| {
                &self.pending[member.index()].name == name
                    && matches!(
                        self.pending[member.index()].kind,
                        PendingKind::Built(crate::DefKind::Let(_))
                    )
            })
        })
    }

    /// Whether `def` is an enum variant.
    pub(crate) fn is_variant(&self, def: DefId) -> bool {
        matches!(
            self.pending[def.index()].kind,
            PendingKind::Built(crate::DefKind::Variant(_))
        )
    }

    /// Whether `def` is a `static` constant an enum or type implementation declares.
    pub(crate) fn is_static(&self, def: DefId) -> bool {
        matches!(
            self.pending[def.index()].kind,
            PendingKind::Built(crate::DefKind::Let(_))
        )
    }

    /// Whether the definition is a table, type or enum: a value type.
    pub(crate) fn is_nominal(&self, def: DefId) -> bool {
        match &self.pending[def.index()].kind {
            PendingKind::Unbuilt(declaration) => matches!(
                **declaration,
                DeclAst::Table(..) | DeclAst::Type(..) | DeclAst::Enum(..)
            ),
            PendingKind::Built(
                crate::DefKind::Table(_) | crate::DefKind::Type(_) | crate::DefKind::Enum(_),
            ) => true,
            PendingKind::Function(_) | PendingKind::Built(_) => false,
        }
    }

    /// Whether the definition is a table.
    pub(crate) fn def_is_table(&self, def: DefId) -> bool {
        match &self.pending[def.index()].kind {
            PendingKind::Unbuilt(declaration) => matches!(**declaration, DeclAst::Table(..)),
            PendingKind::Built(crate::DefKind::Table(_)) => true,
            PendingKind::Function(_) | PendingKind::Built(_) => false,
        }
    }

    /// Whether the definition is a protocol. A generic bound may name only a protocol.
    pub(crate) fn def_is_protocol(&self, def: DefId) -> bool {
        match &self.pending[def.index()].kind {
            PendingKind::Unbuilt(declaration) => matches!(**declaration, DeclAst::Protocol(..)),
            PendingKind::Built(crate::DefKind::Protocol(_)) => true,
            PendingKind::Function(_) | PendingKind::Built(_) => false,
        }
    }

    /// Writes the pending definitions and bodies into the program.
    pub(crate) fn finish(mut self) -> Program {
        let error_ty = self.program.tys.error();
        let pending = std::mem::take(&mut self.pending);
        let mut definitions = IndexVec::with_capacity(pending.len());
        let mut apologies = Vec::new();
        for item in pending {
            let kind = match item.kind {
                PendingKind::Built(kind) => kind,
                PendingKind::Function(function) => DefKind::Fn(FnDef {
                    generics: function.generics,
                    params: function.params,
                    ret: function.ret,
                    effects: function.effects,
                    body: match (function.native, function.body) {
                        (Some(native), _) => FnBody::Native(native),
                        (None, Some(body)) => FnBody::Source(body),
                        (None, None) => FnBody::Signature,
                    },
                }),
                PendingKind::Unbuilt(_) => {
                    apologies.push((item.module, item.span.clone()));
                    DefKind::Fn(FnDef {
                        generics: Vec::new(),
                        params: Vec::new(),
                        ret: error_ty,
                        effects: EffectSet::empty(),
                        body: FnBody::Signature,
                    })
                }
            };
            definitions.push(Def {
                module: item.module,
                name: item.name,
                vis: item.vis,
                span: item.span,
                docs: item.docs,
                clauses: item.clauses,
                kind,
            });
        }
        for (module, span) in apologies {
            self.diagnose(
                module,
                crate::diag::UNSUPPORTED,
                "declaration was not lowered",
                span,
            );
        }
        self.program.defs = definitions;
        // The index promises source order so a query can binary-search by
        // span; lowering visits a callee before its arguments, so sort once.
        for index in 0..self.program.modules.len() {
            let module = &mut self.program.modules[crate::ModuleId::new(index as u32)];
            module.name_uses.sort_by(|left, right| {
                (left.span.start, left.span.end).cmp(&(right.span.start, right.span.end))
            });
        }
        for (index, body) in self.bodies.drain(..).enumerate() {
            let body = body.unwrap_or_else(|| crate::Body {
                owner: self.body_owners[index],
                exprs: IndexVec::new(),
                pats: IndexVec::new(),
                locals: IndexVec::new(),
                params: Box::new([]),
                captures: Box::new([]),
                root: crate::ExprId::new(0),
                effects: EffectSet::empty(),
            });
            self.program.bodies.push(body);
        }
        self.program
    }
}

/// The declared name of a top-level item.
fn item_name(declaration: &Declaration) -> Option<&str> {
    match declaration {
        Declaration::Table { name, .. }
        | Declaration::Protocol { name, .. }
        | Declaration::Enum { name, .. }
        | Declaration::Dimension { name, .. }
        | Declaration::Unit { name, .. }
        | Declaration::Type { name, .. }
        | Declaration::NativeType { name, .. }
        | Declaration::NativeStatic { name, .. } => Some(name),
        Declaration::Function { signature, .. } => Some(&signature.name),
        Declaration::Native { signature, .. } => Some(&signature.name),
        Declaration::Assertion { .. } => Some("assert"),
        Declaration::Let { pattern, .. } => match pattern {
            Pattern::Name(name, _) => Some(name),
            _ => Some("let"),
        },
        Declaration::Use { .. } => None,
    }
}

/// The visibility an item declares.
fn visibility(visibility: &orna_syntax::Visibility) -> Visibility {
    match visibility {
        orna_syntax::Visibility::Public { .. } => Visibility::Public,
        orna_syntax::Visibility::Private => Visibility::Private,
    }
}

/// The origin a module path declares.
fn origin_of(path: &ModulePath) -> ModuleOrigin {
    match path.segments().first().map(Name::as_str) {
        Some("core") => ModuleOrigin::Core,
        Some("sys") => ModuleOrigin::Sys,
        Some("std") => ModuleOrigin::Std,
        _ => ModuleOrigin::Project,
    }
}
