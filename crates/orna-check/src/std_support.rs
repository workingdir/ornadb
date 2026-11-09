//! Standard-library declaration support and fixture constructors.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    AssignmentTarget, Declaration, Expr, Statement, StringSegment, SyntaxTree, UseTail,
};

use super::*;

impl StandardImports {
    pub(crate) fn add(&mut self, path: Vec<String>, tail: &UseTail) {
        match tail {
            UseTail::None => {
                if let Some(last) = path.last().cloned() {
                    self.aliases.insert(last, path);
                }
            }
            UseTail::Alias { name, .. } => {
                self.aliases.insert(name.clone(), path);
            }
            UseTail::Glob { .. } => self.globs.push(path),
            UseTail::Names(names) => {
                for name in names {
                    let mut target = path.clone();
                    target.push(name.name.clone());
                    self.aliases.insert(name.name.clone(), target);
                }
            }
        }
    }
}

/// Propagates host effects through the bodies of source-backed standard
/// functions. A standard wrapper performs whatever host operations its callees
/// perform, so each function's effects are the least fixed point over its
/// syntactic callees. Effects are keyed by full path and monotone, so the
/// iteration terminates.
pub(crate) fn summarize_standard_function_effects(
    modules: &mut BTreeMap<Namespace, ModuleHeader>,
    trees: &[(Namespace, SyntaxTree)],
) {
    let mut imports: BTreeMap<Namespace, StandardImports> = BTreeMap::new();
    let mut functions: BTreeMap<Vec<String>, StandardFunction> = BTreeMap::new();
    for (namespace, tree) in trees {
        let module_imports = imports.entry(namespace.clone()).or_default();
        for item in &tree.items {
            match &item.declaration {
                Declaration::Use { path, tail } => module_imports.add(
                    path.iter().map(|segment| segment.name.clone()).collect(),
                    tail,
                ),
                Declaration::Function { signature, body } => {
                    let mut callees = Vec::new();
                    for parameter in &signature.parameters {
                        if let Some(default) = &parameter.default {
                            collect_callee_paths(default, &mut callees);
                        }
                    }
                    collect_callee_paths(body, &mut callees);
                    let key: Vec<String> = namespace
                        .0
                        .iter()
                        .cloned()
                        .chain([signature.name.clone()])
                        .collect();
                    functions.insert(
                        key,
                        StandardFunction {
                            namespace: namespace.clone(),
                            callees,
                        },
                    );
                }
                _ => {}
            }
        }
    }

    // Declared standard symbols: source functions and host-bound module members.
    // Only these may be named by a namespace-relative callee.
    let declared: BTreeSet<Vec<String>> = modules
        .iter()
        .flat_map(|(namespace, header)| {
            header
                .symbols
                .keys()
                .map(|name| concat_path(&namespace.0, std::slice::from_ref(name)))
                .collect::<Vec<_>>()
        })
        .chain(functions.keys().cloned())
        .collect();
    let candidates: BTreeMap<Vec<String>, Vec<Vec<String>>> = functions
        .iter()
        .map(|(key, function)| {
            let module_imports = imports.get(&function.namespace);
            let paths = function
                .callees
                .iter()
                .flat_map(|callee| {
                    standard_callee_candidates(
                        &function.namespace,
                        module_imports,
                        callee,
                        &declared,
                    )
                })
                .collect();
            (key.clone(), paths)
        })
        .collect();

    // A source function's effects come only from its body: its callees' host
    // classifications and the summaries of the source functions it calls. The
    // path of the function itself is not a classification, so a pure function
    // in a host-named module such as `std.net.*` stays effect-free.
    let mut summary: BTreeMap<Vec<String>, EffectSummary> = functions
        .keys()
        .map(|key| (key.clone(), EffectSummary::default()))
        .collect();
    loop {
        let mut changed = false;
        for (key, paths) in &candidates {
            let mut effects = summary[key].clone();
            for path in paths {
                match summary.get(path) {
                    Some(known) => effects.join(known),
                    None => effects.join(&standard_path_effects(path)),
                }
            }
            if effects != summary[key] {
                summary.insert(key.clone(), effects);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for (key, effect_summary) in summary {
        let Some((name, namespace)) = key.split_last() else {
            continue;
        };
        let Some(header) = modules.get_mut(&Namespace(namespace.to_vec())) else {
            continue;
        };
        for symbols in [&mut header.symbols, &mut header.exports] {
            if let Some(symbol) = symbols.get_mut(name) {
                symbol.effects = effect_summary.clone();
            }
        }
    }
}

/// Effects of a full path that is not a source-declared standard function: its
/// host classification, joined with the descriptor effect of the `sys`
/// operation it names. A `sys` callee carries the descriptor effect it has
/// when called directly, so an opaque `sys.invoke`, `sys.start`, `sys.await` or
/// `sys.cancel` marks its wrapper opaque too (SYS-077, SYS-039).
pub(crate) fn standard_path_effects(path: &[String]) -> EffectSummary {
    let segments = path.iter().map(String::as_str).collect::<Vec<_>>();
    let mut summary = standard_function_effects(&segments);
    if let Some(functions) = system_api::embedded_system_api().function(&path.join(".")) {
        for function in functions {
            summary.join(&descriptor_effects(function.effect));
        }
    }
    summary
}

pub(crate) fn concat_path(prefix: &[String], suffix: &[String]) -> Vec<String> {
    prefix.iter().chain(suffix).cloned().collect()
}

/// Full paths a syntactic callee may denote from within `namespace`. Paths
/// rooted at `std` or `sys` are absolute and always candidates. Any other root
/// is tried relative to the module, its parent, and the module's `use` aliases
/// and globs; a relative spelling is a candidate only when it names a declared
/// symbol, so a parameter or local never inherits a host classification.
pub(crate) fn standard_callee_candidates(
    namespace: &Namespace,
    imports: Option<&StandardImports>,
    callee: &[String],
    declared: &BTreeSet<Vec<String>>,
) -> Vec<Vec<String>> {
    if callee
        .first()
        .is_some_and(|root| root == "std" || root == "sys")
    {
        return vec![callee.to_vec()];
    }
    let mut candidates = Vec::new();
    push_declared(&mut candidates, declared, concat_path(&namespace.0, callee));
    let Some(imports) = imports else {
        return candidates;
    };
    if let Some((root, rest)) = callee.split_first()
        && let Some(target) = imports.aliases.get(root)
    {
        push_import_candidates(&mut candidates, namespace, target, rest, declared);
    }
    for glob in &imports.globs {
        push_import_candidates(&mut candidates, namespace, glob, callee, declared);
    }
    candidates
}

pub(crate) fn push_declared(
    candidates: &mut Vec<Vec<String>>,
    declared: &BTreeSet<Vec<String>>,
    path: Vec<String>,
) {
    if declared.contains(&path) {
        candidates.push(path);
    }
}

pub(crate) fn push_import_candidates(
    candidates: &mut Vec<Vec<String>>,
    namespace: &Namespace,
    base: &[String],
    rest: &[String],
    declared: &BTreeSet<Vec<String>>,
) {
    let target = concat_path(base, rest);
    if base
        .first()
        .is_some_and(|root| root == "std" || root == "sys")
    {
        candidates.push(target);
        return;
    }
    let parent = &namespace.0[..namespace.0.len().saturating_sub(1)];
    push_declared(candidates, declared, concat_path(&namespace.0, &target));
    push_declared(candidates, declared, concat_path(parent, &target));
}

/// Collects the maximal `Name`/`Field` paths an expression references.
/// Paths are syntactic, so they may name callees, function values, or locals;
/// resolution against standard modules happens in `standard_callee_candidates`.
pub(crate) fn collect_callee_paths(expr: &Expr, out: &mut Vec<Vec<String>>) {
    if let Some(path) = qualified_path(expr) {
        out.push(path.into_iter().map(str::to_owned).collect());
        return;
    }
    match expr {
        Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let StringSegment::Expression { value, .. } = segment {
                    collect_callee_paths(value, out);
                }
            }
        }
        Expr::Unary { rhs, .. } => collect_callee_paths(rhs, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_callee_paths(lhs, out);
            collect_callee_paths(rhs, out);
        }
        Expr::Range { lower, upper, .. } => {
            for bound in [lower, upper].into_iter().flatten() {
                collect_callee_paths(bound, out);
            }
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            collect_callee_paths(callee, out);
            for argument in arguments {
                collect_callee_paths(&argument.value, out);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_callee_paths(base, out);
            collect_callee_paths(index, out);
        }
        Expr::Field { base, .. } | Expr::Group { inner: base, .. } => {
            collect_callee_paths(base, out);
        }
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for element in elements {
                collect_callee_paths(element, out);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                collect_callee_paths(&field.value, out);
            }
        }
        Expr::Lambda { body, .. } => collect_callee_paths(body, out),
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                collect_statement_paths(statement, out);
            }
            if let Some(tail) = tail {
                collect_callee_paths(tail, out);
            }
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            for inner in [condition, body, alternate].into_iter().flatten() {
                collect_callee_paths(inner, out);
            }
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_callee_paths(guard, out);
                }
                collect_callee_paths(&arm.body, out);
            }
        }
    }
}

pub(crate) fn collect_statement_paths(statement: &Statement, out: &mut Vec<Vec<String>>) {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. } => collect_callee_paths(value, out),
        Statement::Return { value, .. } | Statement::Break { value, .. } => {
            if let Some(value) = value {
                collect_callee_paths(value, out);
            }
        }
        Statement::Continue { .. } => {}
        Statement::Assignment { target, value, .. } => {
            collect_target_paths(target, out);
            collect_callee_paths(value, out);
        }
    }
}

pub(crate) fn collect_target_paths(target: &AssignmentTarget, out: &mut Vec<Vec<String>>) {
    match target {
        AssignmentTarget::Name { .. } => {}
        AssignmentTarget::Field { base, .. } => collect_target_paths(base, out),
        AssignmentTarget::Index { base, index, .. } => {
            collect_target_paths(base, out);
            collect_callee_paths(index, out);
        }
    }
}

pub(crate) fn fixture_module<I>(namespace: Namespace, symbols: I, implicit: bool) -> ModuleHeader
where
    I: IntoIterator<Item = (String, Symbol)>,
{
    let symbols = symbols
        .into_iter()
        .map(|(name, mut symbol)| {
            if symbol.kind == SymbolKind::Table {
                let prefix = namespace.display();
                symbol.ty = Type::Named(if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                });
            }
            (name, symbol)
        })
        .collect::<BTreeMap<_, _>>();
    ModuleHeader {
        namespace,
        exports: symbols.clone(),
        symbols,
        generic_functions: BTreeMap::new(),
        protocols: BTreeMap::new(),
        prelude_exports: BTreeSet::new(),
        implicit,
    }
}

pub(crate) fn fixture_symbol(kind: SymbolKind, ty: Type) -> Symbol {
    Symbol {
        table_schema: None,
        kind,
        ty,
        public: true,
        effects: EffectSummary::default(),
        generic_parameters: Vec::new(),
        enum_variants: BTreeSet::new(),
    }
}

pub(crate) fn fixture_function(name: &str, ty: Type) -> (String, Symbol) {
    (name.into(), fixture_symbol(SymbolKind::Function, ty))
}

pub(crate) fn fixture_table(name: &str, row: Type) -> (String, Symbol) {
    let mut symbol = fixture_symbol(SymbolKind::Table, Type::Named(name.into()));
    if let Type::Record(fields) = row {
        symbol.table_schema = Some(TableSchema {
            references: Vec::new(),
            declaration_order: fields.keys().cloned().collect(),
            fields,
            admission: None,
            conversions: Vec::new(),
            currency: false,
        });
    }
    (name.into(), symbol)
}

pub(crate) fn fixture_table_with_admission(
    name: &str,
    row: Type,
    admission: TableAdmission,
) -> (String, Symbol) {
    let (name, mut symbol) = fixture_table(name, row);
    if let Some(schema) = &mut symbol.table_schema {
        schema.admission = Some(admission);
    }
    (name, symbol)
}

pub(crate) fn fixture_nominal_type(name: &str, row: Type) -> (String, Symbol) {
    let (name, mut symbol) = fixture_type(name, Type::Named(name.into()));
    if let Type::Record(fields) = row {
        symbol.table_schema = Some(TableSchema {
            references: Vec::new(),
            declaration_order: fields.keys().cloned().collect(),
            fields,
            admission: None,
            conversions: Vec::new(),
            currency: false,
        });
    }
    (name, symbol)
}

/// An attached nominal currency with no public fields. It implements the core
/// `Currency` protocol (ORNA-MONEY-004).
pub(crate) fn fixture_currency(name: &str) -> (String, Symbol) {
    let (name, mut symbol) = fixture_nominal_type(name, Type::Record(BTreeMap::new()));
    if let Some(schema) = &mut symbol.table_schema {
        schema.currency = true;
    }
    (name, symbol)
}

pub(crate) fn fixture_type(name: &str, ty: Type) -> (String, Symbol) {
    (name.into(), fixture_symbol(SymbolKind::Type, ty))
}

pub(crate) fn fixture_connector_function(
    name: &str,
    parameters: Vec<(&str, Type)>,
    result: Type,
) -> (String, Symbol) {
    let (name, mut symbol) = fixture_function(name, named_function(parameters, result));
    symbol.effects = EffectSummary {
        effects: BTreeSet::from([EffectKind::Network]),
        opaque: false,
        may_fail: true,
    };
    (name, symbol)
}

pub(crate) fn fixture_value(name: &str, ty: Type) -> (String, Symbol) {
    (name.into(), fixture_symbol(SymbolKind::Let, ty))
}

pub(crate) fn function(parameters: Vec<Type>, result: Type) -> Type {
    Type::Function {
        parameters,
        parameter_names: None,
        default_parameters: BTreeSet::new(),
        result: Box::new(result),
    }
}

pub(crate) fn named_function(parameters: Vec<(&str, Type)>, result: Type) -> Type {
    let parameter_names = parameters
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    Type::Function {
        parameters: parameters.into_iter().map(|(_, ty)| ty).collect(),
        parameter_names: Some(parameter_names),
        default_parameters: BTreeSet::new(),
        result: Box::new(result),
    }
}
