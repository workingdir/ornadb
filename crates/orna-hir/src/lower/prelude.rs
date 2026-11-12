//! Prelude exports for `use module as _;` (ORNA-IMPORT-005).
//!
//! The HIR does not read the pinned standard profile. The caller states which
//! names each module exports to `use module as _;`, and lowering binds exactly
//! those names. A module without an entry imports no names; it is never a
//! wildcard import.

use std::collections::{BTreeMap, BTreeSet};

use crate::{ModulePath, Name};

/// The names each module exports through `use module as _;`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreludeExports {
    modules: BTreeMap<ModulePath, BTreeSet<Name>>,
}

impl PreludeExports {
    /// Records the prelude names of one module. A repeated path replaces the earlier entry.
    #[must_use]
    pub fn with_module(
        mut self,
        path: ModulePath,
        names: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let names = names
            .into_iter()
            .map(|name| Name::new(name.as_ref()))
            .collect();
        self.modules.insert(path, names);
        self
    }

    /// The prelude names of `path`, when the caller recorded any.
    pub(crate) fn names(&self, path: &ModulePath) -> Option<&BTreeSet<Name>> {
        self.modules.get(path)
    }
}
