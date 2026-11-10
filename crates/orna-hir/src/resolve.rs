//! The one resolver (design sections 1.2, 2.2 and H1).
//!
//! A name is resolved exactly once, here. The resolver answers three kinds of
//! question, all from tables lowering has already built:
//!
//! * **module scope**: which definition does a bare name denote in this module
//!   (a local declaration, an explicit import, a wildcard import, a module
//!   root, or nothing)?
//! * **qualified paths**: which definition does `a.b.c` denote, where `a` is a
//!   module root or alias?
//! * **locals**: which slot does a name denote inside one body? Local scopes
//!   are a stack, innermost first, owned by the body lowerer.
//!
//! The rules are the specification's: local declarations and explicit imports
//! take precedence over wildcard imports ([ORNA-IMPORT-007]); two wildcard
//! imports that expose the same otherwise-unresolved name are ambiguous
//! (ORNA-S011); `sys` is built in and never shadowed (ORNA-NS-006).

use std::collections::{BTreeMap, BTreeSet};

use crate::{DefId, ModuleId, Name};

/// The module-scope bindings of one module.
#[derive(Clone, Debug, Default)]
pub(crate) struct ModuleScope {
    /// Definitions a bare name denotes: local declarations first, then
    /// explicit imports.
    pub(crate) values: BTreeMap<Name, DefId>,
    /// Names the module declares itself, which no import may override.
    pub(crate) local_names: BTreeSet<Name>,
    /// Module roots and aliases, by the name they are written under.
    pub(crate) modules: BTreeMap<Name, ModuleId>,
    /// Names two wildcard imports both provide.
    pub(crate) ambiguous: BTreeSet<Name>,
    /// Wildcard-import candidates by name, before ambiguity is decided.
    pub(crate) glob_candidates: BTreeMap<Name, Vec<DefId>>,
}

impl ModuleScope {
    /// The definition a bare name denotes, ignoring ambiguity.
    pub(crate) fn value(&self, name: &Name) -> Option<DefId> {
        self.values.get(name).copied()
    }

    /// The module a root name denotes.
    pub(crate) fn module(&self, name: &Name) -> Option<ModuleId> {
        self.modules.get(name).copied()
    }
}

/// How a qualified path resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathKind {
    /// The whole path named a definition.
    Def(DefId),
    /// The whole path named a module.
    Module(ModuleId),
    /// The path named a definition, then a member of it (`Note.insert`).
    Member {
        /// The definition the prefix named.
        base: DefId,
        /// The member's index in the written path.
        member: usize,
    },
    /// The path named a module, then a member that module does not declare.
    MissingMember {
        /// The module the member was looked up in.
        module: ModuleId,
        /// The member's index in the written path.
        member: usize,
    },
    /// The root name named no module, so the rest is member access on a value.
    NotAModule,
    /// The root name named an ambiguous wildcard import.
    Ambiguous,
    /// Nothing provides the root name.
    Unresolved,
}
