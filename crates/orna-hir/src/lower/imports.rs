//! Phase 2: import resolution and the module-scope tables (design H1).
//!
//! The rules are `ORNA-IMPORT-001` to `ORNA-IMPORT-007`: local declarations and
//! explicit imports outrank wildcard imports, two wildcard imports that expose
//! the same otherwise-unresolved name are ambiguous (ORNA-S011), and `sys` is
//! built in and never shadowed (ORNA-NS-006). Resolution runs once; every later
//! question is a table lookup.

use std::collections::BTreeMap;

use orna_syntax::UseTail;

use crate::Idx;
use crate::resolve::PathKind;
use crate::{DefId, Import, ImportTarget, ModuleId, ModuleOrigin, ModulePath, Name, NameTarget};

use super::declare::{Builder, PendingUse};

impl Builder {
    /// Resolves every `use` declaration into the importing module's scope.
    pub(crate) fn resolve_imports(&mut self) {
        let uses = std::mem::take(&mut self.uses);
        let mut globs: Vec<(ModuleId, ModuleId, orna_syntax::SyntaxSpan)> = Vec::new();
        for pending in &uses {
            self.apply_use(pending, &mut globs);
        }
        let mut importers: Vec<(ModuleId, orna_syntax::SyntaxSpan)> = Vec::new();
        for (importer, target, span) in globs {
            self.collect_glob(importer, target);
            if !importers.iter().any(|(seen, _)| *seen == importer) {
                importers.push((importer, span));
            }
        }
        for (importer, span) in importers {
            self.finalize_glob(importer, span);
        }
        self.bind_root_environment();
    }

    /// Binds the root-environment names every module sees without an import: `Money` and `Currency`
    /// from `core.money` (spec 09-standard-library.md; hir-host.md B2). A local declaration or an import
    /// that already binds the name wins, so this only fills names the module does not otherwise bind.
    fn bind_root_environment(&mut self) {
        let path = ModulePath::new([Name::new("core"), Name::new("money")]);
        let Some(money) = self.by_path.get(&path).copied() else {
            return;
        };
        let exports = self.program.modules[money].exports.clone();
        for name in ["Money", "Currency"] {
            let name = Name::new(name);
            let Some(def) = exports.get(&name).copied() else {
                continue;
            };
            for scope in &mut self.scopes {
                if scope.local_names.contains(&name)
                    || scope.values.contains_key(&name)
                    || scope.ambiguous.contains(&name)
                    || scope.modules.contains_key(&name)
                {
                    continue;
                }
                scope.values.insert(name.clone(), def);
            }
        }
    }

    /// Applies one `use` declaration.
    fn apply_use(
        &mut self,
        pending: &PendingUse,
        globs: &mut Vec<(ModuleId, ModuleId, orna_syntax::SyntaxSpan)>,
    ) {
        let importer = pending.module;
        let target_path =
            ModulePath::new(pending.path.iter().map(|segment| Name::new(&segment.name)));
        if target_path.segments().first().map(Name::as_str) == Some("sys") {
            self.record_import_path(pending);
            self.apply_builtin_use(pending, &target_path);
            return;
        }
        let Some(target) = self.by_path.get(&target_path).copied() else {
            self.record_import_path(pending);
            self.diagnose(
                importer,
                crate::diag::IMPORT,
                "imported module is unavailable",
                pending.span.clone(),
            );
            return;
        };
        self.record_import_path(pending);
        match &pending.tail {
            UseTail::None => {
                let Some(root) = target_path.segments().last().cloned() else {
                    self.diagnose(
                        importer,
                        crate::diag::IMPORT,
                        "root namespace requires an alias",
                        pending.span.clone(),
                    );
                    return;
                };
                self.bind_root(importer, root, target, pending.span.clone());
            }
            UseTail::Alias { name, span } if name == "_" => {
                self.import_prelude(importer, target_path, target, span.clone());
            }
            UseTail::Alias { name, span } => {
                if name == "sys" {
                    self.diagnose(
                        importer,
                        crate::diag::IMPORT,
                        "the built-in `sys` namespace cannot be shadowed",
                        span.clone(),
                    );
                    return;
                }
                self.bind_root(importer, Name::new(name), target, pending.span.clone());
                super::record_use(
                    &mut self.program,
                    importer,
                    span.clone(),
                    Name::new(name),
                    crate::UseContext::Import,
                    NameTarget::Module(target),
                );
            }
            UseTail::Names(names) => {
                for name in names {
                    let spelling = Name::new(&name.name);
                    match self.program.modules[target].exports.get(&spelling).copied() {
                        Some(def) => {
                            self.bind_explicit(importer, spelling.clone(), def, name.span.clone());
                            self.record_import(
                                importer,
                                target_path.clone(),
                                ImportTarget::Def(def),
                            );
                            super::record_use(
                                &mut self.program,
                                importer,
                                name.span.clone(),
                                spelling,
                                crate::UseContext::Import,
                                NameTarget::Def(def),
                            );
                        }
                        None => {
                            super::record_use(
                                &mut self.program,
                                importer,
                                name.span.clone(),
                                spelling,
                                crate::UseContext::Import,
                                NameTarget::Unresolved,
                            );
                            self.diagnose(
                                importer,
                                crate::diag::IMPORT,
                                "named import is unavailable or private",
                                name.span.clone(),
                            );
                        }
                    }
                }
            }
            UseTail::Glob { .. } => {
                globs.push((importer, target, pending.span.clone()));
                self.record_import(importer, target_path, ImportTarget::Module(target));
            }
        }
    }

    /// A `use sys...[as alias]` declaration: the built-in namespace has no
    /// source export table, so only a root binding can name it.
    fn apply_builtin_use(&mut self, pending: &PendingUse, path: &ModulePath) {
        let sys = self.sys_module;
        match &pending.tail {
            UseTail::None => {
                if let Some(root) = path.segments().last().cloned() {
                    self.bind_root(pending.module, root, sys, pending.span.clone());
                }
            }
            UseTail::Alias { name, .. } if name == "sys" => {
                self.bind_root(pending.module, Name::new(name), sys, pending.span.clone());
            }
            UseTail::Alias { name, span } if name != "_" => {
                self.diagnose(
                    pending.module,
                    crate::diag::IMPORT,
                    "the built-in `sys` namespace cannot be aliased",
                    span.clone(),
                );
            }
            UseTail::Alias { span, .. } | UseTail::Glob { span } => {
                self.diagnose(
                    pending.module,
                    crate::diag::IMPORT,
                    "built-in namespace has no source export table",
                    span.clone(),
                );
            }
            UseTail::Names(names) => {
                let span = names
                    .first()
                    .map_or_else(|| pending.span.clone(), |name| name.span.clone());
                self.diagnose(
                    pending.module,
                    crate::diag::IMPORT,
                    "built-in namespace has no source export table",
                    span,
                );
            }
        }
    }

    /// Binds a module root under `name`.
    fn bind_root(
        &mut self,
        importer: ModuleId,
        name: Name,
        target: ModuleId,
        span: orna_syntax::SyntaxSpan,
    ) {
        let conflict = {
            let scope = &self.scopes[importer.index()];
            scope.values.contains_key(&name)
                || scope
                    .modules
                    .get(&name)
                    .is_some_and(|existing| *existing != target)
        };
        if conflict {
            self.diagnose(
                importer,
                crate::diag::AMBIGUOUS,
                "module import conflicts with an existing binding",
                span,
            );
            return;
        }
        self.scopes[importer.index()].modules.insert(name, target);
        let path = self.program.modules[target].path.clone();
        self.record_import(importer, path, ImportTarget::Module(target));
    }

    /// Binds one explicitly imported name, reporting a conflict.
    fn bind_explicit(
        &mut self,
        importer: ModuleId,
        name: Name,
        def: DefId,
        span: orna_syntax::SyntaxSpan,
    ) {
        let conflict = {
            let scope = &self.scopes[importer.index()];
            !scope.local_names.contains(&name)
                && scope
                    .values
                    .get(&name)
                    .is_some_and(|existing| *existing != def)
        };
        if conflict {
            self.diagnose(
                importer,
                crate::diag::AMBIGUOUS,
                format!("`{name}` is imported twice"),
                span,
            );
            return;
        }
        self.scopes[importer.index()]
            .values
            .entry(name)
            .or_insert(def);
    }

    /// Collects one wildcard import's exports as candidates.
    fn collect_glob(&mut self, importer: ModuleId, target: ModuleId) {
        let exports = self.program.modules[target].exports.clone();
        let scope = &mut self.scopes[importer.index()];
        for (name, def) in exports {
            if scope.local_names.contains(&name) || scope.values.contains_key(&name) {
                continue;
            }
            let candidates = scope.glob_candidates.entry(name).or_default();
            if !candidates.contains(&def) {
                candidates.push(def);
            }
        }
    }

    /// Decides every wildcard-import candidate once all imports are collected.
    fn finalize_glob(&mut self, importer: ModuleId, span: orna_syntax::SyntaxSpan) {
        let candidates = std::mem::take(&mut self.scopes[importer.index()].glob_candidates);
        let mut single: Vec<(Name, DefId)> = Vec::new();
        let mut twice: Vec<Name> = Vec::new();
        for (name, candidates) in candidates {
            match candidates.as_slice() {
                [def] => single.push((name, *def)),
                _ => twice.push(name),
            }
        }
        for name in twice {
            self.scopes[importer.index()].ambiguous.insert(name.clone());
            self.diagnose(
                importer,
                crate::diag::AMBIGUOUS,
                format!("two wildcard imports provide `{name}`"),
                span.clone(),
            );
        }
        for (name, def) in single {
            self.scopes[importer.index()].values.insert(name, def);
        }
    }

    /// Records every segment of a `use` path, resolving each prefix, so an
    /// editor can navigate a module path segment and a name at its end.
    fn record_import_path(&mut self, pending: &PendingUse) {
        let names = pending
            .path
            .iter()
            .map(|segment| Name::new(&segment.name))
            .collect::<Vec<_>>();
        for (index, segment) in pending.path.iter().enumerate() {
            let target = match self.resolve_path(pending.module, &names[..=index]) {
                PathKind::Module(module) => NameTarget::Module(module),
                PathKind::Def(def) => NameTarget::Def(def),
                PathKind::Ambiguous => NameTarget::Ambiguous,
                _ => NameTarget::Unresolved,
            };
            super::record_use(
                &mut self.program,
                pending.module,
                segment.span.clone(),
                names[index].clone(),
                crate::UseContext::Import,
                target,
            );
        }
    }

    /// Records one resolved import on the importing module.
    fn record_import(&mut self, importer: ModuleId, path: ModulePath, target: ImportTarget) {
        self.program.modules[importer]
            .imports
            .push(Import { path, target });
    }

    /// `use module as _;` binds the prelude names recorded for `module` (ORNA-IMPORT-005).
    ///
    /// A module with no recorded prelude imports nothing; this is never a wildcard import.
    fn import_prelude(
        &mut self,
        importer: ModuleId,
        target_path: ModulePath,
        target: ModuleId,
        span: orna_syntax::SyntaxSpan,
    ) {
        self.record_import(importer, target_path.clone(), ImportTarget::Module(target));
        let Some(names) = self.prelude.names(&target_path).cloned() else {
            return;
        };
        for name in names {
            match self.program.modules[target].exports.get(&name).copied() {
                Some(def) => {
                    self.bind_explicit(importer, name.clone(), def, span.clone());
                    self.record_import(importer, target_path.clone(), ImportTarget::Def(def));
                }
                None => self.diagnose(
                    importer,
                    crate::diag::IMPORT,
                    format!("prelude export `{name}` is not public"),
                    span.clone(),
                ),
            }
        }
    }

    /// What a bare name denotes at module scope, ignoring locals.
    pub(crate) fn resolve_bare(&self, module: ModuleId, name: &Name) -> NameTarget {
        let scope = &self.scopes[module.index()];
        if let Some(def) = scope.value(name) {
            return NameTarget::Def(def);
        }
        if scope.ambiguous.contains(name) {
            return NameTarget::Ambiguous;
        }
        if let Some(target) = scope.module(name) {
            return NameTarget::Module(target);
        }
        if super::classify::intrinsic_form(name.as_str()).is_some() {
            return NameTarget::Intrinsic;
        }
        NameTarget::Unresolved
    }

    /// How a qualified path resolves.
    pub(crate) fn resolve_path(&self, module: ModuleId, segments: &[Name]) -> PathKind {
        let Some(first) = segments.first() else {
            return PathKind::Unresolved;
        };
        let scope = &self.scopes[module.index()];
        if let Some(root) = scope.module(first) {
            return self.resolve_from_module(root, &segments[1..]);
        }
        match scope.value(first) {
            Some(def) if segments.len() == 1 => PathKind::Def(def),
            Some(def) => PathKind::Member {
                base: def,
                member: 1,
            },
            None if scope.ambiguous.contains(first) => PathKind::Ambiguous,
            None => PathKind::NotAModule,
        }
    }

    /// Resolves the remainder of a path whose root names `root`.
    fn resolve_from_module(&self, root: ModuleId, rest: &[Name]) -> PathKind {
        if rest.is_empty() {
            return PathKind::Module(root);
        }
        if self.program.modules[root].origin == ModuleOrigin::Sys {
            // A `sys` native is reached by its 1.1 spelling (`sys.blob.from_bytes`), not by its declared name.
            let spelling = self.program.modules[root]
                .path
                .segments()
                .iter()
                .chain(rest)
                .map(Name::as_str)
                .collect::<Vec<_>>()
                .join(".");
            if let Some(def) = self.sys_native_by_spelling(&spelling) {
                return PathKind::Def(def);
            }
        }
        let base = self.program.modules[root].path.clone();
        for take in (1..=rest.len()).rev() {
            let mut candidate: Vec<Name> = base.segments().to_vec();
            candidate.extend(rest[..take].iter().cloned());
            let Some(child) = self.by_path.get(&ModulePath::new(candidate)).copied() else {
                continue;
            };
            let remaining = &rest[take..];
            if remaining.is_empty() {
                return PathKind::Module(child);
            }
            return match self.program.modules[child].exports.get(&remaining[0]) {
                Some(def) if remaining.len() == 1 => PathKind::Def(*def),
                Some(def) => PathKind::Member {
                    base: *def,
                    member: take + 1,
                },
                None => PathKind::MissingMember {
                    module: child,
                    member: take + 1,
                },
            };
        }
        match self.program.modules[root].exports.get(&rest[0]) {
            Some(def) if rest.len() == 1 => PathKind::Def(*def),
            Some(def) => PathKind::Member {
                base: *def,
                member: 1,
            },
            None => PathKind::MissingMember {
                module: root,
                member: 1,
            },
        }
    }
}

/// The module roots every scope binds without an import.
pub(crate) fn implicit_roots(
    std_module: Option<ModuleId>,
    sys_module: ModuleId,
) -> BTreeMap<Name, ModuleId> {
    let mut roots = BTreeMap::new();
    roots.insert(Name::new("sys"), sys_module);
    if let Some(std) = std_module {
        roots.insert(Name::new("std"), std);
    }
    roots
}
