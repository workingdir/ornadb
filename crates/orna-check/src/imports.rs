//! Import resolution and item dispatch.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use orna_syntax_ast::{
    Declaration, Item, ProtocolMember, SyntaxTree, TypeExpr, TypeMember, TypeRepresentation,
    UseTail,
};
use orna_value::Diagnostic;

use super::*;

pub(crate) fn resolve_imports(
    namespace: &Namespace,
    tree: &SyntaxTree,
    header: &ModuleHeader,
    modules: &BTreeMap<Namespace, ModuleHeader>,
    historical_modules: &BTreeMap<Namespace, ModuleHeader>,
    attached_symbols: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Scope {
    resolve_imports_with_dependencies(
        namespace,
        tree,
        header,
        ImportInputs {
            modules,
            historical_modules,
            attached_symbols,
            attached_databases: &BTreeSet::new(),
            dependency_summaries: &BTreeMap::new(),
            units: &UnitEnvironment::default(),
        },
        diagnostics,
    )
}

pub(crate) fn resolve_imports_with_dependencies(
    namespace: &Namespace,
    tree: &SyntaxTree,
    header: &ModuleHeader,
    inputs: ImportInputs<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Scope {
    let ImportInputs {
        modules,
        attached_symbols,
        dependency_summaries,
        ..
    } = &inputs;
    let mut scope = module_scope(namespace, tree, header, &inputs, diagnostics);
    install_attached_symbols(&mut scope, attached_symbols);
    install_module_rows(&mut scope, modules);
    let mut implicit_roots = register_implicit_modules(&mut scope, modules);
    let (explicit, glob) =
        collect_use_bindings(&mut scope, tree, modules, &mut implicit_roots, diagnostics);
    apply_use_bindings(&mut scope, explicit, glob, diagnostics);
    import_protocol_members(&mut scope, tree, modules);
    import_dependency_provenance(&mut scope, tree, modules, dependency_summaries);
    canonicalize_scope_metadata(&mut scope, namespace, tree);
    scope.currency_types = currency_types(namespace, tree, &scope);
    install_attached_type_rows(&mut scope, attached_symbols);
    collect_protocol_implementations(&mut scope, tree);

    collect_nominal_conversion_effects(&mut scope, tree);
    merge_exported_conversion_metadata(&mut scope, modules);
    scope
}

pub(crate) fn module_scope(
    namespace: &Namespace,
    tree: &SyntaxTree,
    header: &ModuleHeader,
    inputs: &ImportInputs<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Scope {
    Scope {
        names: header.symbols.clone(),
        current_namespace: None,
        source_units: Rc::clone(&inputs.units.units),
        source_dimensions: Rc::clone(&inputs.units.dimensions),
        function_dependencies: RefCell::new(BTreeMap::new()),
        unlinked_system_members: None,
        ambiguous: BTreeSet::new(),
        nominal_identities: BTreeMap::new(),
        modules: BTreeMap::new(),
        available_modules: inputs.modules.clone(),
        historical_modules: inputs.historical_modules.clone(),
        attached_databases: inputs.attached_databases.clone(),
        // Row shapes and automatic-key tables are keyed by the same qualified
        // identity that table symbols carry, never by the bare declared name.
        table_rows: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Table { name, .. } => {
                    Some((nominal_identity(namespace, name), table_row_type(item)))
                }
                _ => None,
            })
            .collect(),
        auto_key_tables: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Table { name, keys, .. } if keys.is_empty() => {
                    Some(nominal_identity(namespace, name))
                }
                _ => None,
            })
            .collect(),
        nominal_rows: tree.items.iter().filter_map(nominal_row_type).collect(),
        nominal_private_fields: BTreeMap::new(),
        secret_types: BTreeSet::new(),
        private_field_owner: None,
        allow_local_private_nominal_construction: false,
        local_nominal_names: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Type {
                    name,
                    representation: TypeRepresentation::Nominal { .. },
                    ..
                } => Some(name.clone()),
                _ => None,
            })
            .collect(),
        refined_types: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Type {
                    name,
                    representation: TypeRepresentation::Alias { ty, refinements },
                    ..
                } if refinements
                    .iter()
                    .any(|member| matches!(member, TypeMember::Assertion { .. })) =>
                {
                    Some((name.clone(), type_of(ty)))
                }
                _ => None,
            })
            .collect(),
        currency_types: BTreeSet::new(),
        enum_variants: enum_variant_types(tree, diagnostics),
        payload_enum_types: payload_enum_names(tree),
        imported_enum_variants: BTreeMap::new(),
        type_aliases: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Type {
                    name,
                    generics,
                    representation: TypeRepresentation::Alias { ty, refinements },
                } if generics.is_empty() && refinements.is_empty() => {
                    Some((name.clone(), type_of(ty)))
                }
                _ => None,
            })
            .collect(),
        generic_protocols: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Protocol { name, generics, .. } if !generics.is_empty() => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect(),
        local_protocols: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Protocol {
                    name,
                    generics,
                    members,
                } if generics.is_empty() => Some((name.clone(), members.clone())),
                _ => None,
            })
            .collect(),
        generic_type_parameters: BTreeSet::new(),
        generic_functions: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Function { signature, .. } if !signature.generics.is_empty() => {
                    Some((signature.name.clone(), signature.clone()))
                }
                _ => None,
            })
            .collect(),
        generic_type_bounds: BTreeMap::new(),
        protocol_implementations: BTreeMap::new(),
        nominal_conversions: BTreeMap::new(),
        nominal_conversion_effects: BTreeMap::new(),
        return_frames: RefCell::new(Vec::new()),
    }
}

pub(crate) fn install_attached_symbols(
    scope: &mut Scope,
    attached_symbols: &BTreeMap<String, Symbol>,
) {
    for (name, symbol) in attached_symbols {
        scope
            .names
            .entry(name.clone())
            .or_insert_with(|| symbol.clone());
        if symbol.kind == SymbolKind::Table
            && let Some(row) = symbol.table_fields()
        {
            scope
                .table_rows
                .entry(name.clone())
                .or_insert_with(|| Type::Record(row.clone()));
        } else if symbol.kind == SymbolKind::Type
            && let Some(row) = symbol.table_fields()
        {
            let identity = match &symbol.ty {
                Type::Named(identity) => identity.clone(),
                _ => name.clone(),
            };
            scope
                .nominal_rows
                .entry(identity)
                .or_insert_with(|| Type::Record(row.clone()));
        }
    }
}

pub(crate) fn install_module_rows(scope: &mut Scope, modules: &BTreeMap<Namespace, ModuleHeader>) {
    for (module_namespace, module) in modules {
        for (name, symbol) in &module.exports {
            if symbol.kind != SymbolKind::Table {
                continue;
            }
            let Some(row) = symbol.table_fields() else {
                continue;
            };
            let identity = match &symbol.ty {
                Type::Named(identity) => identity.clone(),
                _ => nominal_identity(module_namespace, name),
            };
            scope
                .table_rows
                .entry(identity)
                .or_insert_with(|| Type::Record(row.clone()));
        }
        for (name, symbol) in &module.exports {
            if symbol.kind != SymbolKind::Type {
                continue;
            }
            let Some(fields) = symbol.table_fields() else {
                continue;
            };
            // Exported nominal metadata contains public fields only. Local
            // rows were installed above and therefore retain their complete
            // private representation; imported rows remain an admission
            // surface, never a way to recover private fields.
            scope
                .nominal_rows
                .entry(name.clone())
                .or_insert_with(|| Type::Record(fields.clone()));
        }
    }
}

pub(crate) fn register_implicit_modules(
    scope: &mut Scope,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) -> BTreeSet<String> {
    if modules.contains_key(&Namespace(vec!["std".into()])) {
        scope
            .modules
            .insert("std".into(), Namespace(vec!["std".into()]));
    }
    for (module_namespace, module) in modules {
        if module.implicit && module_namespace.0.len() == 1 && !module_namespace.0[0].is_empty() {
            scope
                .modules
                .entry(module_namespace.0[0].clone())
                .or_insert_with(|| module_namespace.clone());
        }
    }
    scope.modules.keys().cloned().collect::<BTreeSet<_>>()
}

pub(crate) fn collect_use_bindings(
    scope: &mut Scope,
    tree: &SyntaxTree,
    modules: &BTreeMap<Namespace, ModuleHeader>,
    implicit_roots: &mut BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (BTreeMap<String, Symbol>, BTreeMap<String, Vec<Symbol>>) {
    let mut explicit = BTreeMap::<String, Symbol>::new();
    let mut glob = BTreeMap::<String, Vec<Symbol>>::new();
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let target = Namespace(path.iter().map(|x| x.name.clone()).collect());
        if target.0.first().is_some_and(|r| r == "sys") {
            import_builtin_namespace(&target, tail, &mut explicit, diagnostics);
            continue;
        }
        let Some(module) = modules.get(&target) else {
            diagnostics.push(diag(DIAG_IMPORT, "imported module is unavailable"));
            continue;
        };
        match tail {
            UseTail::None => {
                let Some(name) = target.0.last() else {
                    diagnostics.push(diag(DIAG_IMPORT, "root namespace requires an alias"));
                    continue;
                };
                bind_module_root(
                    scope,
                    implicit_roots,
                    name,
                    &target,
                    &mut explicit,
                    diagnostics,
                );
            }
            UseTail::Alias { name, .. } if name == "_" => {
                import_prelude_exports(module, &mut explicit, diagnostics);
            }
            UseTail::Alias { name, .. } => {
                if rejects_portable_sys_shadow(name, diagnostics) {
                    continue;
                }
                bind_module_root(
                    scope,
                    implicit_roots,
                    name,
                    &target,
                    &mut explicit,
                    diagnostics,
                );
            }
            UseTail::Names(names) => {
                import_named_symbols(module, names, &mut explicit, diagnostics)
            }
            UseTail::Glob { .. } => import_glob_exports(module, &mut glob),
        }
    }
    (explicit, glob)
}

/// The reserved `sys` namespace has no source export table; only a root or
/// alias binding can name it (ORNA-NS-001).
pub(crate) fn import_builtin_namespace(
    target: &Namespace,
    tail: &UseTail,
    explicit: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let binding = Symbol {
        table_schema: None,
        kind: SymbolKind::Let,
        ty: Type::Named(target.display()),
        public: true,
        effects: EffectSummary::default(),
        generic_parameters: Vec::new(),
        enum_variants: BTreeSet::new(),
    };
    match tail {
        UseTail::None => {
            if let Some(name) = target.0.last() {
                insert_explicit(explicit, name.clone(), binding, diagnostics);
            }
        }
        UseTail::Alias { name, .. } if name != "_" => {
            insert_explicit(explicit, name.clone(), binding, diagnostics);
        }
        UseTail::Alias { .. } | UseTail::Glob { .. } | UseTail::Names(_) => {
            diagnostics.push(diag(
                DIAG_IMPORT,
                "built-in namespace has no source export table",
            ));
        }
    }
}

/// Binds one module root under `name`, or reports the conflict that keeps the
/// earlier binding. A name already owned by another root drops the implicit
/// root so later resolution cannot see both.
pub(crate) fn bind_module_root(
    scope: &mut Scope,
    implicit_roots: &mut BTreeSet<String>,
    name: &str,
    target: &Namespace,
    explicit: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if scope.names.contains_key(name)
        || explicit.contains_key(name)
        || scope.modules.get(name).is_some_and(|root| root != target)
    {
        diagnostics.push(diag(
            DIAG_AMBIGUOUS,
            "module import conflicts with an existing binding",
        ));
        if implicit_roots.remove(name) {
            scope.modules.remove(name);
        }
        return;
    }
    scope.modules.insert(name.to_owned(), target.clone());
    implicit_roots.remove(name);
    insert_explicit(
        explicit,
        name.to_owned(),
        Symbol {
            table_schema: None,
            kind: SymbolKind::Let,
            ty: Type::Named(target.display()),
            public: true,
            effects: EffectSummary::default(),
            generic_parameters: Vec::new(),
            enum_variants: BTreeSet::new(),
        },
        diagnostics,
    );
}

/// `use module as _` imports the module's declared prelude exports.
pub(crate) fn import_prelude_exports(
    module: &ModuleHeader,
    explicit: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for name in &module.prelude_exports {
        if let Some(symbol) = module.exports.get(name) {
            insert_explicit(explicit, name.clone(), symbol.clone(), diagnostics);
        } else {
            diagnostics.push(diag(DIAG_IMPORT, "prelude export is not public"));
        }
    }
}

pub(crate) fn import_named_symbols(
    module: &ModuleHeader,
    names: &[orna_syntax_ast::ImportSegment],
    explicit: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for import in names {
        if let Some(symbol) = module.exports.get(&import.name) {
            insert_explicit(explicit, import.name.clone(), symbol.clone(), diagnostics);
        } else {
            diagnostics.push(diag(DIAG_IMPORT, "named import is unavailable or private"));
        }
    }
}

/// A wildcard import collects candidates; `apply_use_bindings` reports the
/// ambiguous names once every module has contributed.
pub(crate) fn import_glob_exports(module: &ModuleHeader, glob: &mut BTreeMap<String, Vec<Symbol>>) {
    for (name, symbol) in &module.exports {
        glob.entry(name.clone()).or_default().push(symbol.clone());
    }
}

pub(crate) fn apply_use_bindings(
    scope: &mut Scope,
    explicit: BTreeMap<String, Symbol>,
    glob: BTreeMap<String, Vec<Symbol>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (name, symbol) in explicit {
        scope.names.entry(name).or_insert(symbol);
    }
    for (name, candidates) in glob {
        if rejects_portable_sys_shadow(&name, diagnostics) {
            continue;
        }
        if scope.names.contains_key(&name) {
            continue;
        }
        if candidates.len() == 1 {
            scope.names.insert(name, candidates[0].clone());
        } else {
            scope.ambiguous.insert(name);
        }
    }
}

pub(crate) fn import_protocol_members(
    scope: &mut Scope,
    tree: &SyntaxTree,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) {
    // Import the protocol member signatures alongside named or wildcard
    // imports so implementations are checked against captured source.
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let target = Namespace(path.iter().map(|segment| segment.name.clone()).collect());
        let Some(module) = modules.get(&target) else {
            continue;
        };
        let names = match tail {
            UseTail::Names(names) => names.iter().map(|name| name.name.clone()).collect(),
            UseTail::Glob { .. } => module.protocols.keys().cloned().collect(),
            UseTail::Alias { name, .. } if name == "_" => module
                .prelude_exports
                .iter()
                .filter(|name| module.protocols.contains_key(*name))
                .cloned()
                .collect(),
            UseTail::None | UseTail::Alias { .. } => Vec::new(),
        };
        for name in names {
            if scope
                .names
                .get(&name)
                .is_some_and(|symbol| symbol.kind == SymbolKind::Protocol)
                && let Some(members) = module.protocols.get(&name)
            {
                scope
                    .local_protocols
                    .entry(name)
                    .or_insert_with(|| members.clone());
            }
        }
    }
}

pub(crate) fn import_dependency_provenance(
    scope: &mut Scope,
    tree: &SyntaxTree,
    modules: &BTreeMap<Namespace, ModuleHeader>,
    dependency_summaries: &BTreeMap<Namespace, BTreeMap<String, BTreeSet<String>>>,
) {
    // Preserve dependency provenance for imported helpers without extending
    // the public ModuleHeader/Symbol representations. Qualified calls use the
    // module binding (including aliases); named and wildcard imports use their
    // lexical binding.
    for (alias, target) in scope.modules.clone() {
        let Some(summaries) = dependency_summaries.get(&target) else {
            continue;
        };
        for (name, dependencies) in summaries {
            scope
                .function_dependencies
                .borrow_mut()
                .insert(format!("{alias}.{name}"), dependencies.clone());
        }
    }
    // A helper reached through its module's full namespace path
    // (`mail.google.sync`) keeps the same provenance; an explicit binding of
    // the same spelling inserted above takes precedence.
    for (target, summaries) in dependency_summaries {
        if target.0.is_empty() {
            continue;
        }
        let prefix = target.display();
        for (name, dependencies) in summaries {
            scope
                .function_dependencies
                .borrow_mut()
                .entry(format!("{prefix}.{name}"))
                .or_insert_with(|| dependencies.clone());
        }
    }
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let target = Namespace(path.iter().map(|x| x.name.clone()).collect());
        let Some(module) = modules.get(&target) else {
            continue;
        };
        let mut imported = Vec::new();
        match tail {
            UseTail::Names(names) => {
                imported.extend(names.iter().map(|name| name.name.clone()));
            }
            UseTail::Glob { .. } => {
                imported.extend(
                    module
                        .exports
                        .iter()
                        .filter(|(_, symbol)| symbol.kind == SymbolKind::Function)
                        .map(|(name, _)| name.clone()),
                );
            }
            UseTail::None => {}
            UseTail::Alias { name, .. } if name == "_" => {}
            UseTail::Alias { name, .. } => {
                if scope.modules.get(name) == Some(&target) {
                    for function in module
                        .exports
                        .iter()
                        .filter(|(_, symbol)| symbol.kind == SymbolKind::Function)
                        .map(|(name, _)| name)
                    {
                        if let Some(dependencies) = dependency_summaries
                            .get(&target)
                            .and_then(|summaries| summaries.get(function))
                        {
                            scope
                                .function_dependencies
                                .borrow_mut()
                                .insert(format!("{name}.{function}"), dependencies.clone());
                        }
                    }
                }
                continue;
            }
        }
        for name in imported {
            let Some(symbol) = module.exports.get(&name) else {
                continue;
            };
            if scope.names.get(&name) != Some(symbol) {
                continue;
            }
            if let Some(dependencies) = dependency_summaries
                .get(&target)
                .and_then(|summaries| summaries.get(&name))
            {
                scope
                    .function_dependencies
                    .borrow_mut()
                    .insert(name, dependencies.clone());
            }
        }
    }
}

pub(crate) fn install_attached_type_rows(
    scope: &mut Scope,
    attached_symbols: &BTreeMap<String, Symbol>,
) {
    for (name, symbol) in attached_symbols {
        if symbol.kind == SymbolKind::Type
            && let Some(row) = symbol.table_fields()
        {
            let identity = match &symbol.ty {
                Type::Named(identity) => identity.clone(),
                _ => name.clone(),
            };
            scope
                .nominal_rows
                .entry(identity)
                .or_insert_with(|| Type::Record(row.clone()));
        }
    }
}

pub(crate) fn collect_protocol_implementations(scope: &mut Scope, tree: &SyntaxTree) {
    for item in &tree.items {
        let (target, members) = match &item.declaration {
            Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { members },
                ..
            } => (
                name,
                members
                    .iter()
                    .filter_map(|member| match member {
                        TypeMember::Implementation { implementation, .. } => Some(implementation),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            Declaration::Type {
                name,
                representation: TypeRepresentation::Alias { refinements, .. },
                ..
            } => (
                name,
                refinements
                    .iter()
                    .filter_map(|member| match member {
                        TypeMember::Implementation { implementation, .. } => Some(implementation),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            Declaration::Table { name, members, .. } => (
                name,
                members
                    .iter()
                    .filter_map(|member| match member {
                        orna_syntax_ast::TableMember::Implementation { implementation, .. } => {
                            Some(implementation)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => continue,
        };
        let target_identity = canonical_nominal_name(target, scope);
        let protocols = members
            .iter()
            .map(|implementation| resolved_type_of(&implementation.protocol, scope))
            .filter(|protocol| *protocol != Type::Error)
            .collect::<Vec<_>>();
        scope
            .protocol_implementations
            .entry(target_identity.clone())
            .or_default()
            .extend(protocols);
        for implementation in members {
            let TypeExpr::Name {
                path, arguments, ..
            } = &implementation.protocol
            else {
                continue;
            };
            if path.as_slice() != ["From"] || arguments.len() != 1 {
                continue;
            }
            if let Some(source) = valid_static_type(&arguments[0], scope) {
                scope
                    .nominal_conversions
                    .entry(target_identity.clone())
                    .or_default()
                    .push(source);
            }
        }
    }
}

pub(crate) fn collect_nominal_conversion_effects(scope: &mut Scope, tree: &SyntaxTree) {
    for item in &tree.items {
        let (target, implementations) = match &item.declaration {
            Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { members },
                ..
            } => (
                name,
                members
                    .iter()
                    .filter_map(|member| match member {
                        TypeMember::Implementation { implementation, .. } => Some(implementation),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            Declaration::Type {
                name,
                representation: TypeRepresentation::Alias { refinements, .. },
                ..
            } => (
                name,
                refinements
                    .iter()
                    .filter_map(|member| match member {
                        TypeMember::Implementation { implementation, .. } => Some(implementation),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            Declaration::Table { name, members, .. } => (
                name,
                members
                    .iter()
                    .filter_map(|member| match member {
                        orna_syntax_ast::TableMember::Implementation { implementation, .. } => {
                            Some(implementation)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => continue,
        };
        let target = canonical_nominal_name(target, scope);
        for implementation in implementations {
            let TypeExpr::Name {
                path, arguments, ..
            } = &implementation.protocol
            else {
                continue;
            };
            if path.as_slice() != ["From"] || arguments.len() != 1 {
                continue;
            }
            let Some(source) = valid_static_type(&arguments[0], scope) else {
                continue;
            };
            let Some(orna_syntax_ast::ImplMember::Function {
                signature, body, ..
            }) = implementation.members.as_slice().first()
            else {
                continue;
            };

            if implementation.members.len() != 1
                || signature.name != "from"
                || signature.parameters.len() != 1
            {
                continue;
            }
            let source_binding = match &source {
                Type::Named(name) => scope
                    .nominal_rows
                    .get(name)
                    .or_else(|| scope.table_rows.get(name))
                    .cloned()
                    .unwrap_or_else(|| source.clone()),
                _ => source.clone(),
            };
            let mut local = BTreeMap::new();
            let mut ignored = Vec::new();
            bind_pattern(
                &signature.parameters[0].pattern,
                source_binding,
                scope,
                &mut local,
                &mut ignored,
            );
            let inferred = infer(body, scope, &local, &mut ignored);
            scope
                .nominal_conversion_effects
                .entry(target.clone())
                .or_default()
                .push((source, inferred.effects));
        }
    }
}

pub(crate) fn conversion_metadata(scope: &Scope) -> BTreeMap<String, Vec<(Type, EffectSummary)>> {
    let mut metadata = BTreeMap::new();
    for (target, sources) in &scope.nominal_conversions {
        let entries = sources
            .iter()
            .map(|source| {
                let effects = scope
                    .nominal_conversion_effects
                    .get(target)
                    .and_then(|candidates| {
                        candidates
                            .iter()
                            .find(|(candidate, _)| candidate == source)
                            .map(|(_, effects)| effects.clone())
                    })
                    .unwrap_or_default();
                (source.clone(), effects)
            })
            .collect::<Vec<_>>();
        metadata.insert(target.clone(), entries);
    }
    metadata
}

pub(crate) fn attach_conversion_metadata(
    symbols: &mut BTreeMap<String, Symbol>,
    metadata: &BTreeMap<String, Vec<(Type, EffectSummary)>>,
) {
    for symbol in symbols.values_mut() {
        if symbol.kind != SymbolKind::Type {
            continue;
        }
        let Type::Named(target) = &symbol.ty else {
            continue;
        };
        let Some(schema) = symbol.table_schema.as_mut() else {
            continue;
        };
        schema.conversions = metadata.get(target).cloned().unwrap_or_default();
    }
}

/// Export which local nominal types implement the core `Currency` protocol,
/// so an importer accepts them as currencies without reading their source.
/// Resolve stored references from table columns to other tables.
///
/// A table type used as a field denotes a stored reference to a row of that
/// table (ORNA-REF-001). Resolution runs after every table in the module is
/// declared, because the target is named by symbol.
///
/// Actions follow ORNA-REF-003: a non-optional reference defaults to
/// `restrict` on delete and re-key. An optional reference may hold no value,
/// so it defaults to `set_none` on both actions. Automatic cascade is outside
/// version 1.0 and is never produced.
pub(crate) fn attach_table_references(symbols: &mut BTreeMap<String, Symbol>) {
    let tables: BTreeSet<String> = symbols
        .values()
        .filter(|symbol| symbol.kind == SymbolKind::Table)
        .filter_map(|symbol| match &symbol.ty {
            Type::Named(name) => Some(name.clone()),
            _ => None,
        })
        .collect();
    for symbol in symbols.values_mut() {
        if symbol.kind != SymbolKind::Table {
            continue;
        }
        let Some(schema) = symbol.table_schema.as_mut() else {
            continue;
        };
        let mut references = Vec::new();
        for (column, ty) in &schema.fields {
            let (target, optional) = match ty {
                Type::Named(target) => (target, false),
                Type::Optional(inner) => match inner.as_ref() {
                    Type::Named(target) => (target, true),
                    _ => continue,
                },
                _ => continue,
            };
            if !tables.contains(target) {
                continue;
            }
            let action = if optional {
                ReferenceAction::SetNone
            } else {
                ReferenceAction::Restrict
            };
            references.push(TableReference {
                from_column: column.clone(),
                to_table: target.clone(),
                optional,
                on_delete: action,
                on_rekey: action,
            });
        }
        schema.references = references;
    }
}

pub(crate) fn attach_currency_metadata(
    symbols: &mut BTreeMap<String, Symbol>,
    currencies: &BTreeSet<String>,
) {
    for symbol in symbols.values_mut() {
        if symbol.kind != SymbolKind::Type {
            continue;
        }
        let Type::Named(identity) = &symbol.ty else {
            continue;
        };
        let currency = currencies.contains(identity);
        if let Some(schema) = symbol.table_schema.as_mut() {
            schema.currency = currency;
        }
    }
}

pub(crate) fn merge_exported_conversion_metadata(
    scope: &mut Scope,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) {
    for module in modules.values() {
        for symbol in module.exports.values() {
            if symbol.kind != SymbolKind::Type {
                continue;
            }
            let Type::Named(target) = &symbol.ty else {
                continue;
            };
            let Some(schema) = symbol.table_schema.as_ref() else {
                continue;
            };
            for (source, effects) in &schema.conversions {
                let sources = scope.nominal_conversions.entry(target.clone()).or_default();
                if !sources.contains(source) {
                    sources.push(source.clone());
                }
                let effect_entries = scope
                    .nominal_conversion_effects
                    .entry(target.clone())
                    .or_default();
                if !effect_entries
                    .iter()
                    .any(|(candidate, _)| candidate == source)
                {
                    effect_entries.push((source.clone(), effects.clone()));
                }
            }
        }
    }
}

pub(crate) fn insert_explicit(
    map: &mut BTreeMap<String, Symbol>,
    name: String,
    symbol: Symbol,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if rejects_portable_sys_shadow(&name, diagnostics) {
        return;
    }
    if map.insert(name, symbol).is_some() {
        diagnostics.push(diag(DIAG_AMBIGUOUS, "conflicting explicit imports"));
    }
}

pub(crate) fn check_item(
    item: &Item,
    source_order: usize,
    symbols: &mut BTreeMap<String, Symbol>,
    scope: &Scope,
    _table_rows: &BTreeMap<String, Type>,
    plans: &mut Vec<AssertionPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if let Some((name, _, _)) = declared_symbol(item)
        && name == "sys"
        && !matches!(&item.declaration, Declaration::Table { .. })
    {
        diagnostics.push(diag(
            DIAG_RESERVED,
            "`sys` is an implementation-provided namespace and cannot be shadowed",
        ));
        return None;
    }
    match &item.declaration {
        Declaration::Let {
            pattern,
            annotation,
            value,
        } => {
            if let Some(expected) = annotation
                .as_ref()
                .map(|annotation| resolved_type_of(annotation, scope))
            {
                let inferred =
                    infer_contextual(value, &expected, scope, &BTreeMap::new(), diagnostics);
                require_same(&expected, &inferred.ty, diagnostics);
                bind_pattern(pattern, inferred.ty.clone(), scope, symbols, diagnostics);
                return Some(inferred);
            } else {
                let inferred = infer(value, scope, &BTreeMap::new(), diagnostics);
                bind_pattern(pattern, inferred.ty.clone(), scope, symbols, diagnostics);
                return Some(inferred);
            }
        }
        Declaration::Function { signature, .. } => {
            check_function(item, symbols, scope, diagnostics);
            return symbols.get(&signature.name).map(|symbol| Inferred {
                ty: symbol.ty.clone(),
                effects: symbol.effects.clone(),
            });
        }
        Declaration::Protocol { name, members, .. } if name == "Currency" => {
            for member in members {
                if let ProtocolMember::Static { name, .. } = member
                    && name == "symbol"
                {
                    diagnostics.push(diag(
                        DIAG_CURRENCY_SYMBOL,
                        "currency symbols belong to locale-aware formatting, not Currency identity",
                    ));
                }
            }
        }
        Declaration::Assertion { value } => {
            let inferred = infer_module_assertion(value, &scope.table_rows, scope, diagnostics);
            assertion(
                AssertionInput {
                    owner: AssertionOwner::Module,
                    value,
                    inferred,
                    resolved_tables: Some(&scope.table_rows),
                    scope: Some(scope),
                    // A module assertion is evaluated over the unpublished
                    // candidate database before publication (ORNA-ASSERT-027).
                    enforcement: AssertionScope::SnapshotCandidate,
                    source_order,
                },
                plans,
                diagnostics,
            );
        }
        Declaration::Table {
            name,
            keys,
            members,
            ..
        } => check_table_item(
            TableCheck {
                item,
                name,
                keys,
                members,
                symbols,
                scope,
                plans,
            },
            diagnostics,
        ),
        Declaration::Type {
            name,
            representation: TypeRepresentation::Alias { ty, refinements },
            ..
        } => check_alias_type_item(name, ty, refinements, scope, plans, diagnostics),
        Declaration::Type {
            name,
            representation: TypeRepresentation::Nominal { members },
            ..
        } => check_nominal_type_item(name, members, scope, diagnostics),
        // ORNA-NATIVE-008 and ORNA-NATIVE-010: the declaration's own signature
        // and effect clause type and classify a native call. Stage N3 reads
        // them; stage N1 admits the syntax only. A native declaration has no
        // body, contributes no symbol here and is never read as a function.
        Declaration::Native { .. } => {}
        _ => {}
    }
    None
}
