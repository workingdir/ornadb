//! The program, its modules and their resolved imports (design section 2.2).

use std::collections::BTreeMap;

use crate::{
    Body, BodyId, Def, DefId, EffectSet, FnBody, HirDiagnostic, IndexVec, ModuleId, ModulePath,
    Name, NameUse, NativeAbi, SourceId, TyInterner,
};

/// Where a module came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleOrigin {
    /// The `core` declarations: language-defined natives.
    Core,
    /// The `sys` declarations.
    Sys,
    /// The pinned standard library.
    Std,
    /// A module of the program being checked.
    Project,
    /// A module entered in the REPL.
    Repl,
}

/// One resolved import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Import {
    /// The imported path as written.
    pub path: ModulePath,
    /// What it names.
    pub target: ImportTarget,
}

/// What one import names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportTarget {
    /// One definition.
    Def(DefId),
    /// One module.
    Module(ModuleId),
}

/// One module.
#[derive(Clone, Debug)]
pub struct Module {
    /// Its path, for example `std.bits`.
    pub path: ModulePath,
    /// Where it came from.
    pub origin: ModuleOrigin,
    /// The source text its spans refer to.
    pub source: SourceId,
    /// Its definitions, in declaration order.
    pub items: Vec<DefId>,
    /// Its exports, by name.
    pub exports: BTreeMap<Name, DefId>,
    /// Its imports, already resolved.
    pub imports: Vec<Import>,
    /// Diagnostics produced while lowering this module.
    pub diagnostics: Vec<HirDiagnostic>,
    /// Every name occurrence in this module and what it denotes, in source
    /// order. This is the index the language server and the resolution
    /// comparison read (design H1).
    pub name_uses: Vec<NameUse>,
}

/// One lowered program: every module, definition, body and type of a
/// compilation.
#[derive(Clone, Debug)]
pub struct Program {
    /// The modules, in admission order.
    pub modules: IndexVec<ModuleId, Module>,
    /// The definitions, global across modules.
    pub defs: IndexVec<DefId, Def>,
    /// The bodies.
    pub bodies: IndexVec<BodyId, Body>,
    /// The interned types.
    pub tys: TyInterner,
    /// Each type's protocol implementations, in declaration order: the
    /// candidates a member call on a value of that type selects from.
    pub instances: BTreeMap<DefId, Vec<DefId>>,
    /// Which engine native ABI the `core` and `sys` declarations were loaded
    /// against.
    pub native_abi: NativeAbi,
}

impl Program {
    /// An empty program: no modules, definitions, bodies or types.
    ///
    /// The native ABI is `0` until lowering admits the `core` and `sys`
    /// declarations, which is where the engine's ABI number is recorded.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            modules: IndexVec::new(),
            defs: IndexVec::new(),
            bodies: IndexVec::new(),
            tys: TyInterner::new(),
            instances: BTreeMap::new(),
            native_abi: NativeAbi::new(0),
        }
    }

    /// Whether the program holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty() && self.defs.is_empty() && self.bodies.is_empty()
    }

    /// The body a definition's signature owns when the definition has one.
    #[must_use]
    pub fn source_body(&self, def: DefId) -> Option<BodyId> {
        match self.defs.get(def)?.kind {
            crate::DefKind::Fn(crate::FnDef {
                body: FnBody::Source(body),
                ..
            }) => Some(body),
            _ => None,
        }
    }

    /// The effects recorded on a definition's body, when it has one.
    #[must_use]
    pub fn body_effects(&self, def: DefId) -> Option<EffectSet> {
        self.bodies
            .get(self.source_body(def)?)
            .map(|body| body.effects)
    }
}
