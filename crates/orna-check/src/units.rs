//! Source unit and dimension algebra.

use std::collections::{BTreeMap, BTreeSet};

use num_bigint::BigInt;
use num_traits::{One, ToPrimitive};
use orna_syntax::{
    Declaration, Expr, Item, LiteralKind, Pattern, SyntaxTree, TypeExpr, TypeMember,
    TypeRepresentation, UnitDefinition, Visibility,
};
use orna_value::Diagnostic;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::{UnicodeNormalization, is_nfc};

use super::*;

/// Resolves dimension and unit declarations to their reduced dimension bases
/// and unit metadata. Declarations that cannot be reduced are omitted, so
/// every use of them stays unresolved instead of guessing from a spelling.
pub(crate) fn analyze_source_units(
    items: &[(Namespace, String, Item)],
) -> (
    BTreeMap<SourceUnitIdentity, SourceUnitMetadata>,
    BTreeMap<SourceDimensionIdentity, DimensionBasis>,
) {
    let mut dimensions = BTreeMap::new();
    let mut units = BTreeMap::new();
    let mut duplicate_dimensions = BTreeSet::new();
    let mut duplicate_units = BTreeSet::new();
    for (namespace, logical_path, item) in items {
        match &item.declaration {
            Declaration::Dimension { name, expression } => {
                let identity = SourceDimensionIdentity {
                    namespace: namespace.clone(),
                    name: name.clone(),
                };
                if dimensions
                    .insert(
                        identity.clone(),
                        SourceDimensionDecl {
                            expression: expression.clone(),
                        },
                    )
                    .is_some()
                {
                    duplicate_dimensions.insert(identity);
                }
            }
            Declaration::Unit {
                name,
                dimension,
                definition,
            } => {
                let identity = SourceUnitIdentity {
                    namespace: namespace.clone(),
                    name: name.clone(),
                };
                let declaration = SourceUnitDecl {
                    identity: identity.clone(),
                    dimension: dimension.clone(),
                    definition: definition.clone(),
                    provenance: SourceUnitProvenance {
                        logical_path: logical_path.clone(),
                        span: item.span.clone(),
                    },
                };
                if units.insert(identity.clone(), declaration).is_some() {
                    duplicate_units.insert(identity);
                }
            }
            _ => {}
        }
    }
    for identity in duplicate_dimensions {
        dimensions.remove(&identity);
    }
    for identity in duplicate_units {
        units.remove(&identity);
    }

    let mut result = BTreeMap::new();
    for declaration in units.values() {
        let mut resolving_units = BTreeSet::new();
        let Some(scale) = source_unit_scale(&declaration.identity, &units, &mut resolving_units)
        else {
            continue;
        };
        let (offset, affine) = match &declaration.definition {
            UnitDefinition::Base => (UnitFraction::zero(), false),
            UnitDefinition::Derived { offset, affine, .. } => {
                let offset = match offset {
                    Some(expression) => match source_numeric_value(expression) {
                        Some(value) => value,
                        None => continue,
                    },
                    None => UnitFraction::zero(),
                };
                (offset, *affine)
            }
        };
        let Some(dimension) = source_dimension_path(&declaration.dimension) else {
            continue;
        };
        let Some(dimension_identity) =
            source_dimension_identity(&declaration.identity.namespace, &dimension, &dimensions)
        else {
            continue;
        };
        let mut resolving_dimensions = BTreeSet::new();
        let Some(dimension_basis) =
            source_dimension_basis(&dimension_identity, &dimensions, &mut resolving_dimensions)
        else {
            continue;
        };
        result.insert(
            declaration.identity.clone(),
            SourceUnitMetadata {
                identity: declaration.identity.clone(),
                dimension_basis,
                scale: scale.public(),
                offset: offset.public(),
                affine,
                provenance: declaration.provenance.clone(),
            },
        );
    }
    let bases = dimensions
        .keys()
        .filter_map(|identity| {
            source_dimension_basis(identity, &dimensions, &mut BTreeSet::new())
                .map(|basis| (identity.clone(), basis))
        })
        .collect();
    (result, bases)
}

pub(crate) fn source_dimension_path(ty: &TypeExpr) -> Option<Vec<String>> {
    match ty {
        TypeExpr::Name {
            path, arguments, ..
        } if arguments.is_empty() => Some(path.clone()),
        _ => None,
    }
}

pub(crate) fn source_dimension_identity(
    namespace: &Namespace,
    path: &[String],
    dimensions: &BTreeMap<SourceDimensionIdentity, SourceDimensionDecl>,
) -> Option<SourceDimensionIdentity> {
    let name = path.last()?.clone();
    let candidate = if path.len() == 1 {
        SourceDimensionIdentity {
            namespace: namespace.clone(),
            name: name.clone(),
        }
    } else {
        SourceDimensionIdentity {
            namespace: Namespace(path[..path.len() - 1].to_vec()),
            name: name.clone(),
        }
    };
    if dimensions.contains_key(&candidate) {
        return Some(candidate);
    }
    let mut matches = dimensions.keys().filter(|identity| identity.name == name);
    let identity = matches.next()?.clone();
    matches.next().is_none().then_some(identity)
}

pub(crate) fn source_dimension_basis(
    identity: &SourceDimensionIdentity,
    dimensions: &BTreeMap<SourceDimensionIdentity, SourceDimensionDecl>,
    stack: &mut BTreeSet<SourceDimensionIdentity>,
) -> Option<BTreeMap<SourceDimensionIdentity, i64>> {
    if !stack.insert(identity.clone()) {
        return None;
    }
    let declaration = dimensions.get(identity)?;
    let result = if let Some(expression) = &declaration.expression {
        let mut result = BTreeMap::new();
        for (index, (_, ty, exponent, _)) in expression.terms.iter().enumerate() {
            let path = source_dimension_path(ty)?;
            let target = source_dimension_identity(&identity.namespace, &path, dimensions)?;
            let mut power = match exponent {
                Some(exponent) => exponent.parse::<i64>().ok()?,
                None => 1,
            };
            if index > 0
                && expression
                    .operators
                    .get(index - 1)
                    .is_some_and(|operator| operator == "/")
            {
                power = power.checked_neg()?;
            }
            let term = source_dimension_basis(&target, dimensions, stack)?;
            for (axis, exponent) in term {
                let combined = result
                    .get(&axis)
                    .copied()
                    .unwrap_or(0i64)
                    .checked_add(exponent.checked_mul(power)?)?;
                if combined == 0 {
                    result.remove(&axis);
                } else {
                    result.insert(axis, combined);
                }
            }
        }
        result
    } else {
        BTreeMap::from([(identity.clone(), 1)])
    };
    stack.remove(identity);
    Some(result)
}

pub(crate) fn source_unit_identity(
    namespace: &Namespace,
    name: &str,
    units: &BTreeMap<SourceUnitIdentity, SourceUnitDecl>,
) -> Option<SourceUnitIdentity> {
    let candidate = SourceUnitIdentity {
        namespace: namespace.clone(),
        name: name.to_owned(),
    };
    if units.contains_key(&candidate) {
        return Some(candidate);
    }
    let mut matches = units.keys().filter(|identity| identity.name == name);
    let identity = matches.next()?.clone();
    matches.next().is_none().then_some(identity)
}

pub(crate) fn source_unit_scale(
    identity: &SourceUnitIdentity,
    units: &BTreeMap<SourceUnitIdentity, SourceUnitDecl>,
    stack: &mut BTreeSet<SourceUnitIdentity>,
) -> Option<UnitFraction> {
    if !stack.insert(identity.clone()) {
        return None;
    }
    let declaration = units.get(identity)?;
    let value = match &declaration.definition {
        UnitDefinition::Base => Some(UnitFraction::one()),
        UnitDefinition::Derived { value, .. } => {
            source_scale_expression(value, &identity.namespace, units, stack)
        }
    };
    stack.remove(identity);
    value
}

pub(crate) fn source_numeric_value(expression: &Expr) -> Option<UnitFraction> {
    match expression {
        Expr::Literal {
            text,
            kind: LiteralKind::Integer | LiteralKind::Decimal | LiteralKind::Float,
            ..
        } => UnitFraction::decimal(text),
        Expr::Group { inner, .. } => source_numeric_value(inner),
        Expr::Unary { op, rhs, .. } if op == "+" || op == "-" => {
            let mut value = source_numeric_value(rhs)?;
            if op == "-" {
                value.numerator = -value.numerator;
            }
            Some(value)
        }
        Expr::Binary { lhs, op, rhs, .. } if op == "+" || op == "-" || op == "*" || op == "/" => {
            let left = source_numeric_value(lhs)?;
            let right = source_numeric_value(rhs)?;
            match op.as_str() {
                "+" | "-" => {
                    let sign = if op == "+" {
                        BigInt::one()
                    } else {
                        -BigInt::one()
                    };
                    UnitFraction::new(
                        &left.numerator * &right.denominator
                            + sign * &right.numerator * &left.denominator,
                        &left.denominator * &right.denominator,
                    )
                }
                "*" => left.multiply(&right),
                "/" => left.divide(&right),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(crate) fn source_scale_expression(
    expression: &Expr,
    namespace: &Namespace,
    units: &BTreeMap<SourceUnitIdentity, SourceUnitDecl>,
    stack: &mut BTreeSet<SourceUnitIdentity>,
) -> Option<UnitFraction> {
    match expression {
        Expr::Literal {
            text,
            kind: LiteralKind::Integer | LiteralKind::Decimal | LiteralKind::Float,
            ..
        } => UnitFraction::decimal(text),
        Expr::Group { inner, .. } => source_scale_expression(inner, namespace, units, stack),
        Expr::Name { text, .. } => {
            let identity = source_unit_identity(namespace, text, units)?;
            source_unit_scale(&identity, units, stack)
        }
        Expr::Field { base, name, .. } => {
            let base = source_scale_expression(base, namespace, units, stack)?;
            let identity = source_unit_identity(namespace, name, units)?;
            base.multiply(&source_unit_scale(&identity, units, stack)?)
        }
        Expr::Unary { op, rhs, .. } if op == "+" || op == "-" => {
            let mut value = source_scale_expression(rhs, namespace, units, stack)?;
            if op == "-" {
                value.numerator = -value.numerator;
            }
            Some(value)
        }
        Expr::Binary { lhs, op, rhs, .. } => {
            if op == "^" {
                let base = source_scale_expression(lhs, namespace, units, stack)?;
                let exponent = source_numeric_value(rhs)?;
                if exponent.denominator != BigInt::one() {
                    return None;
                }
                return base.power(exponent.numerator.to_i64()?);
            }
            let left = source_scale_expression(lhs, namespace, units, stack)?;
            let right = source_scale_expression(rhs, namespace, units, stack)?;
            match op.as_str() {
                "*" => left.multiply(&right),
                "/" => left.divide(&right),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Function bodies may depend on inferred summaries declared later in their
/// module or exported by a later project input. Iterate the finite declaration
/// graph before emitting diagnostics so public result and effect summaries are
/// independent of manifest order.
pub(crate) fn stabilize_function_summaries(
    parsed: &[(Namespace, SyntaxTree)],
    modules: &mut BTreeMap<Namespace, ModuleHeader>,
    historical_modules: &BTreeMap<Namespace, ModuleHeader>,
    attached_symbols: &BTreeMap<String, Symbol>,
    attached_databases: &BTreeSet<String>,
    dependency_summaries: &mut BTreeMap<Namespace, BTreeMap<String, BTreeSet<String>>>,
    units: &UnitEnvironment,
) {
    let pass_limit = parsed
        .iter()
        .map(|(_, tree)| tree.items.len())
        .sum::<usize>()
        .saturating_add(1);
    for _ in 0..pass_limit {
        let before = modules.clone();
        let dependencies_before = dependency_summaries.clone();
        for (namespace, tree) in parsed {
            let Some(header) = modules.get(namespace).cloned() else {
                continue;
            };
            let mut discarded = Vec::new();
            let scope = resolve_imports_with_dependencies(
                namespace,
                tree,
                &header,
                ImportInputs {
                    modules,
                    historical_modules,
                    attached_symbols,
                    attached_databases,
                    dependency_summaries,
                    units,
                },
                &mut discarded,
            );
            let mut symbols = header.symbols;
            for symbol in symbols.values_mut() {
                canonicalize_symbol(symbol, &scope.nominal_identities);
                normalize_symbol_quantities(symbol, &scope);
            }
            for item in &tree.items {
                if matches!(item.declaration, Declaration::Function { .. }) {
                    check_function(item, &mut symbols, &scope, &mut discarded);
                }
            }
            let summaries = scope.function_dependencies.borrow();
            let local_summaries = dependency_summaries.entry(namespace.clone()).or_default();
            for item in &tree.items {
                let Declaration::Function { signature, .. } = &item.declaration else {
                    continue;
                };
                if let Some(dependencies) = summaries.get(&signature.name) {
                    local_summaries.insert(signature.name.clone(), dependencies.clone());
                }
            }
            let conversion_metadata = conversion_metadata(&scope);
            attach_conversion_metadata(&mut symbols, &conversion_metadata);
            attach_currency_metadata(&mut symbols, &scope.currency_types);
            attach_table_references(&mut symbols);
            if let Some(module) = modules.get_mut(namespace) {
                module.symbols = symbols;
                module.exports = module
                    .symbols
                    .iter()
                    .filter(|(_, symbol)| symbol.public && !type_contains_error(&symbol.ty))
                    .map(|(name, symbol)| (name.clone(), symbol.clone()))
                    .collect();
            }
        }
        if *modules == before && *dependency_summaries == dependencies_before {
            break;
        }
    }
}

pub(crate) fn stabilize_local_function_dependencies(tree: &SyntaxTree, scope: &Scope) {
    let pass_limit = tree.items.len().saturating_add(1);
    for _ in 0..pass_limit {
        let before = scope.function_dependencies.borrow().clone();
        for item in &tree.items {
            let Declaration::Function { signature, body } = &item.declaration else {
                continue;
            };
            let parameter_names = signature
                .parameters
                .iter()
                .flat_map(|parameter| pattern_binding_names(&parameter.pattern))
                .collect::<BTreeSet<_>>();
            let dependencies = tables_referenced(
                body,
                Some(&scope.table_rows),
                Some(scope),
                Some(&parameter_names),
            );
            scope
                .function_dependencies
                .borrow_mut()
                .insert(signature.name.clone(), dependencies);
        }
        if *scope.function_dependencies.borrow() == before {
            break;
        }
    }
}

/// Host-independent NFKC case fold key used for sibling collision checking.
pub(crate) fn unicode_sibling_key(value: &str) -> String {
    value.nfkc().case_fold().collect()
}

pub(crate) fn namespace_for_path(
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Namespace> {
    if path.is_empty() || path.starts_with('/') || path.contains("\\") || !path.ends_with(".orna") {
        diagnostics.push(diag(
            DIAG_BAD_PATH,
            "module path must be a relative .orna path",
        ));
        return None;
    }
    let mut parts = path.split('/').collect::<Vec<_>>();
    let file = parts.pop()?;
    if parts
        .iter()
        .any(|part| part.is_empty() || matches!(*part, "." | "..") || !is_nfc(part))
        || !is_nfc(file)
    {
        diagnostics.push(diag(
            DIAG_BAD_PATH,
            "module path components must be NFC logical components",
        ));
        return None;
    }
    let stem = file.strip_suffix(".orna")?;
    if stem.is_empty() || stem.contains('.') {
        diagnostics.push(diag(
            DIAG_BAD_PATH,
            "module filename has no valid logical stem",
        ));
        return None;
    }
    let mut names = parts.into_iter().map(str::to_owned).collect::<Vec<_>>();
    if stem != "main" {
        names.push(stem.to_owned());
    }
    Some(Namespace(names))
}

pub(crate) fn collect_header(
    namespace: &Namespace,
    tree: &SyntaxTree,
    prelude: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ModuleHeader {
    let mut symbols = BTreeMap::new();
    // Natives carry no symbol: they bind at link. Their names still occupy the module, so a repeat is a duplicate.
    let mut native_names = BTreeSet::new();
    for item in &tree.items {
        let Some((name, kind, mut ty)) = declared_symbol(item) else {
            if let Declaration::Native { signature, .. } = &item.declaration
                && (!native_names.insert(signature.name.clone())
                    || symbols.contains_key(&signature.name))
            {
                diagnostics.push(diag(DIAG_DUPLICATE, "duplicate declaration name"));
            }
            continue;
        };
        // Nominal types, tables and units carry their module-qualified identity, so
        // rows of `a.Item` and `b.Item` are never the same row type (08-relations:
        // row values compare table ObjectId; ORNA-ROW-001), and imports keep their
        // declaring module rather than a bare spelling.
        if matches!(
            &item.declaration,
            Declaration::Type {
                representation: TypeRepresentation::Nominal { .. },
                ..
            } | Declaration::Table { .. }
                | Declaration::Unit { .. }
        ) {
            ty = Type::Named(nominal_identity(namespace, &name));
        }
        // `sys` is the implementation-provided namespace and must not enter
        // a source module's header under any declaration kind.  Table `sys`
        // retains its dedicated type diagnostic in `check_item`.
        if name == "sys" {
            continue;
        }
        // A native binds at link and has no symbol here, but its name still belongs to this module.
        if native_names.contains(&name) {
            diagnostics.push(diag(DIAG_DUPLICATE, "duplicate declaration name"));
        }
        let public = matches!(item.visibility, Visibility::Public { .. });
        if symbols
            .insert(
                name,
                Symbol {
                    table_schema: declared_table_schema(item)
                        .or_else(|| declared_nominal_schema(item)),
                    kind,
                    ty,
                    public,
                    effects: EffectSummary::default(),
                    generic_parameters: declared_generic_parameters(item),
                    enum_variants: declared_enum_variants(item),
                },
            )
            .is_some()
        {
            // A second declaration under one name is an identity collision: the
            // symbol table keeps the first and the second is rejected.
            diagnostics.push(diag(DIAG_DUPLICATE, "duplicate declaration name"));
        }
    }
    let exports = symbols
        .iter()
        .filter(|(_, s)| s.public && !type_contains_error(&s.ty))
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();
    ModuleHeader {
        namespace: namespace.clone(),
        exports,
        symbols,
        prelude_exports: prelude.clone(),
        generic_functions: tree
            .items
            .iter()
            .filter_map(|item| {
                let Declaration::Function { signature, .. } = &item.declaration else {
                    return None;
                };
                (!signature.generics.is_empty())
                    .then(|| (signature.name.clone(), declared_generic_parameters(item)))
            })
            .collect(),
        protocols: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Protocol {
                    name,
                    generics,
                    members,
                } if generics.is_empty()
                    && matches!(item.visibility, Visibility::Public { .. }) =>
                {
                    Some((name.clone(), members.clone()))
                }
                _ => None,
            })
            .collect(),
        implicit: false,
    }
}

/// `sys` is an implementation-provided portable root, not an ordinary source
/// symbol. Reject every attempted source admission before it can enter a
/// header, import scope, or local binding table.
pub(crate) fn rejects_portable_sys_shadow(name: &str, diagnostics: &mut Vec<Diagnostic>) -> bool {
    if name != "sys" {
        return false;
    }
    diagnostics.push(diag(
        DIAG_RESERVED,
        "`sys` is an implementation-provided namespace and cannot be shadowed",
    ));
    true
}

pub(crate) fn declared_generic_parameters(item: &Item) -> Vec<GenericParameterMetadata> {
    let Declaration::Function { signature, .. } = &item.declaration else {
        return Vec::new();
    };
    signature
        .generics
        .iter()
        .map(|generic| GenericParameterMetadata {
            name: generic.name.clone(),
            bounds: generic.bounds.iter().map(type_of).collect(),
        })
        .collect()
}

pub(crate) fn declared_symbol(item: &Item) -> Option<(String, SymbolKind, Type)> {
    match &item.declaration {
        Declaration::Function { signature, body } => {
            let parameters = signature
                .parameters
                .iter()
                .map(|p| {
                    p.annotation.as_ref().map(type_of).unwrap_or_else(|| {
                        inferred_function_parameter_type(body, p).unwrap_or(Type::Error)
                    })
                })
                .collect();
            let parameter_names = signature
                .parameters
                .iter()
                .map(|parameter| match &parameter.pattern {
                    Pattern::Name(name, _) => Some(name.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            Some((
                signature.name.clone(),
                SymbolKind::Function,
                Type::Function {
                    parameters,
                    parameter_names,
                    default_parameters: signature
                        .parameters
                        .iter()
                        .enumerate()
                        .filter_map(|(index, parameter)| parameter.default.as_ref().map(|_| index))
                        .collect(),
                    result: Box::new(
                        signature
                            .result
                            .as_ref()
                            .map(type_of)
                            .unwrap_or(Type::Error),
                    ),
                },
            ))
        }
        Declaration::Table { name, .. } => Some((
            name.clone(),
            SymbolKind::Table,
            Type::Named(name.to_owned()),
        )),
        Declaration::Type { name, .. } => {
            Some((name.clone(), SymbolKind::Type, Type::Named(name.to_owned())))
        }
        Declaration::Enum { name, .. } => {
            Some((name.clone(), SymbolKind::Enum, Type::Named(name.to_owned())))
        }
        Declaration::Protocol { name, .. } => Some((
            name.clone(),
            SymbolKind::Protocol,
            Type::Named(name.to_owned()),
        )),
        Declaration::Dimension { name, .. } => Some((
            name.clone(),
            SymbolKind::Dimension,
            Type::Named(name.to_owned()),
        )),
        Declaration::Unit { name, .. } => {
            Some((name.clone(), SymbolKind::Unit, Type::Named(name.to_owned())))
        }
        Declaration::Let {
            pattern: Pattern::Name(name, _),
            annotation,
            ..
        } => Some((
            name.clone(),
            SymbolKind::Let,
            annotation.as_ref().map(type_of).unwrap_or(Type::Error),
        )),
        // ORNA-NATIVE-008: a native declaration's signature is its type, and
        // there is no body to infer parameter types from. Stage N3 installs
        // the symbol from the declaration; stage N1 records none, so a native
        // name resolves to nothing rather than to a fabricated function type.
        Declaration::Native { .. } => None,
        _ => None,
    }
}

pub(crate) fn declared_enum_variants(item: &Item) -> BTreeSet<String> {
    let Declaration::Enum { variants, .. } = &item.declaration else {
        return BTreeSet::new();
    };
    variants
        .iter()
        .map(|variant| variant.name.clone())
        .collect()
}

/// Converts one declared source type expression to the unresolved semantic
/// type recorded by table and nominal declarations. Named types keep their
/// source spelling; callers resolve them against the analysed module scope.
pub fn declared_type(ty: &TypeExpr) -> Type {
    type_of(ty)
}

pub(crate) fn type_of(ty: &TypeExpr) -> Type {
    match ty {
        TypeExpr::Name {
            path, arguments, ..
        } if arguments.is_empty() => {
            primitive(&path.join(".")).unwrap_or_else(|| Type::Named(path.join(".")))
        }
        TypeExpr::Name {
            path, arguments, ..
        } if path.as_slice() == ["Relation"] && arguments.len() == 1 => {
            Type::Relation(Box::new(type_of(&arguments[0])))
        }
        TypeExpr::Name {
            path, arguments, ..
        } if path.as_slice() == ["Query"] && arguments.len() == 1 => {
            // Query<T> is the read-only planner handle for the same typed row
            // stream represented by Relation<T> in the current evaluator.
            Type::Relation(Box::new(type_of(&arguments[0])))
        }
        TypeExpr::Name {
            path, arguments, ..
        } if path.as_slice() == ["Range"] && arguments.len() == 1 => {
            Type::Range(Box::new(type_of(&arguments[0])))
        }
        TypeExpr::Name {
            path, arguments, ..
        } => Type::Applied {
            base: path.join("."),
            arguments: arguments.iter().map(type_of).collect(),
        },
        TypeExpr::List { inner, .. } => Type::List(Box::new(type_of(inner))),
        TypeExpr::Record { fields, .. } => Type::Record(
            fields
                .iter()
                .map(|(n, t, _)| (n.clone(), type_of(t)))
                .collect(),
        ),
        TypeExpr::Tuple { elements, .. } => Type::Tuple(elements.iter().map(type_of).collect()),
        TypeExpr::Function {
            parameters, result, ..
        } => Type::Function {
            parameters: parameters.iter().map(type_of).collect(),
            parameter_names: None,
            default_parameters: BTreeSet::new(),
            result: Box::new(type_of(result)),
        },
        TypeExpr::Optional { inner, .. } => Type::Optional(Box::new(type_of(inner))),
        TypeExpr::Product { lhs, op, rhs, .. } => {
            let lhs = type_of(lhs);
            let rhs = type_of(rhs);
            match (lhs, op.as_str(), rhs) {
                (Type::Applied { base, arguments }, "/", unit)
                    if base == "Money" && arguments.len() == 1 =>
                {
                    Type::MoneyPerUnit {
                        currency: Box::new(arguments.into_iter().next().unwrap_or(Type::Error)),
                        unit: Box::new(unit),
                    }
                }
                // A compound unit such as the `m / s` of `Float<m / s>`. It
                // resolves against declared units when the type is
                // canonicalized; anywhere else it is not a type.
                (lhs, "*" | "/", rhs) if is_unit_term(&lhs) && is_unit_term(&rhs) => {
                    Type::Applied {
                        base: op.clone(),
                        arguments: vec![lhs, rhs],
                    }
                }
                _ => Type::Error,
            }
        }
    }
}

/// A unit name or compound unit as `type_of` records it inside a type.
pub(crate) fn is_unit_term(ty: &Type) -> bool {
    match ty {
        Type::Named(_) => true,
        Type::Applied { base, arguments } => {
            matches!(base.as_str(), "*" | "/") && arguments.iter().all(is_unit_term)
        }
        _ => false,
    }
}

pub(crate) fn resolved_type_of(ty: &TypeExpr, scope: &Scope) -> Type {
    let mut resolving = BTreeSet::new();
    let resolved = resolve_type_aliases(&type_of(ty), &scope.type_aliases, &mut resolving);
    canonicalize_type(&resolved, scope)
}

pub(crate) fn resolve_type_aliases(
    ty: &Type,
    aliases: &BTreeMap<String, Type>,
    resolving: &mut BTreeSet<String>,
) -> Type {
    match ty {
        Type::Named(name) => {
            let Some(alias) = aliases.get(name) else {
                return ty.clone();
            };
            if !resolving.insert(name.clone()) {
                return Type::Error;
            }
            let resolved = resolve_type_aliases(alias, aliases, resolving);
            resolving.remove(name);
            resolved
        }
        Type::List(inner) => Type::List(Box::new(resolve_type_aliases(inner, aliases, resolving))),
        Type::Range(inner) => {
            Type::Range(Box::new(resolve_type_aliases(inner, aliases, resolving)))
        }
        Type::Relation(inner) => {
            Type::Relation(Box::new(resolve_type_aliases(inner, aliases, resolving)))
        }
        Type::Stream(inner) => {
            Type::Stream(Box::new(resolve_type_aliases(inner, aliases, resolving)))
        }
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        resolve_type_aliases(field, aliases, resolving),
                    )
                })
                .collect(),
        ),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| resolve_type_aliases(element, aliases, resolving))
                .collect(),
        ),
        Type::Optional(inner) => {
            Type::Optional(Box::new(resolve_type_aliases(inner, aliases, resolving)))
        }
        Type::Applied { base, arguments } => Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| resolve_type_aliases(argument, aliases, resolving))
                .collect(),
        },
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            offset,
            affine,
        } => Type::Quantity {
            numeric: Box::new(resolve_type_aliases(numeric, aliases, resolving)),
            dimension_basis: dimension_basis.clone(),
            scale: scale.clone(),
            offset: offset.clone(),
            affine: *affine,
        },
        Type::MoneyPerUnit { currency, unit } => Type::MoneyPerUnit {
            currency: Box::new(resolve_type_aliases(currency, aliases, resolving)),
            unit: Box::new(resolve_type_aliases(unit, aliases, resolving)),
        },
        Type::Function {
            parameters,
            parameter_names,
            default_parameters,
            result,
        } => Type::Function {
            parameters: parameters
                .iter()
                .map(|parameter| resolve_type_aliases(parameter, aliases, resolving))
                .collect(),
            parameter_names: parameter_names.clone(),
            default_parameters: default_parameters.clone(),
            result: Box::new(resolve_type_aliases(result, aliases, resolving)),
        },
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Bottom
        | Type::Error => ty.clone(),
    }
}

pub(crate) fn canonicalize_type(ty: &Type, scope: &Scope) -> Type {
    let canonical = canonicalize_type_with_identities(ty, &scope.nominal_identities);
    normalize_source_quantities(&canonical, scope, None)
}

/// Rewrites `Int<m>`, `Float<m / s>`, `Quantity<Float, m>` and the unit of a
/// money rate to structural quantities resolved from declared units
/// (ORNA-UNIT-001). `home` names the module that wrote the type when that is
/// not the module of `scope`, such as the declaring module of an imported
/// table row. A unit that does not resolve stays as written, for annotation
/// validation to report.
pub(crate) fn normalize_source_quantities(
    ty: &Type,
    scope: &Scope,
    home: Option<&Namespace>,
) -> Type {
    let normalize = |ty: &Type| normalize_source_quantities(ty, scope, home);
    match ty {
        Type::List(inner) => Type::List(Box::new(normalize(inner))),
        Type::Range(inner) => Type::Range(Box::new(normalize(inner))),
        Type::Relation(inner) => Type::Relation(Box::new(normalize(inner))),
        Type::Stream(inner) => Type::Stream(Box::new(normalize(inner))),
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), normalize(field)))
                .collect(),
        ),
        Type::Tuple(elements) => Type::Tuple(elements.iter().map(normalize).collect()),
        Type::Optional(inner) => Type::Optional(Box::new(normalize(inner))),
        Type::Applied { base, arguments } => {
            let quantity = match (base.as_str(), arguments.as_slice()) {
                ("Int", [unit]) => unit_quantity_type(Type::Int, unit, scope, home),
                ("Decimal", [unit]) => unit_quantity_type(Type::Decimal, unit, scope, home),
                ("Float", [unit]) => unit_quantity_type(Type::Float, unit, scope, home),
                ("Quantity", [numeric @ (Type::Int | Type::Decimal | Type::Float), unit]) => {
                    unit_quantity_type(numeric.clone(), unit, scope, home)
                }
                _ => None,
            };
            quantity.unwrap_or_else(|| Type::Applied {
                base: base.clone(),
                arguments: arguments.iter().map(normalize).collect(),
            })
        }
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            offset,
            affine,
        } => Type::Quantity {
            numeric: Box::new(normalize(numeric)),
            dimension_basis: dimension_basis.clone(),
            scale: scale.clone(),
            offset: offset.clone(),
            affine: *affine,
        },
        // Money is exact, so the rate's unit is recorded as a Decimal quantity.
        Type::MoneyPerUnit { currency, unit } => Type::MoneyPerUnit {
            currency: Box::new(normalize(currency)),
            unit: Box::new(
                unit_quantity_type(Type::Decimal, unit, scope, home)
                    .unwrap_or_else(|| normalize(unit)),
            ),
        },
        Type::Function {
            parameters,
            parameter_names,
            default_parameters,
            result,
        } => Type::Function {
            parameters: parameters.iter().map(normalize).collect(),
            parameter_names: parameter_names.clone(),
            default_parameters: default_parameters.clone(),
            result: Box::new(normalize(result)),
        },
        _ => ty.clone(),
    }
}

/// The quantity type of `numeric` values measured in `unit`, a unit name or
/// compound unit recorded by `type_of`.
pub(crate) fn unit_quantity_type(
    numeric: Type,
    unit: &Type,
    scope: &Scope,
    home: Option<&Namespace>,
) -> Option<Type> {
    let shape = unit_shape(unit, scope, home)?;
    // A compound unit that cancels completely, such as `m / m`, is a plain
    // number, as the matching product of quantities is.
    if shape.dimension_basis.is_empty() && shape.scale == UnitFraction::one() && !shape.affine {
        return Some(numeric);
    }
    Some(Type::Quantity {
        numeric: Box::new(numeric),
        dimension_basis: shape.dimension_basis,
        scale: shape.scale.public(),
        offset: shape.offset.public(),
        affine: shape.affine,
    })
}

pub(crate) fn unit_shape(
    unit: &Type,
    scope: &Scope,
    home: Option<&Namespace>,
) -> Option<UnitShape> {
    match unit {
        Type::Named(name) => {
            let path = name.split('.').map(str::to_owned).collect::<Vec<_>>();
            let declared = home
                .filter(|_| path.len() == 1)
                .and_then(|home| {
                    scope.source_units.get(&SourceUnitIdentity {
                        namespace: home.clone(),
                        name: name.clone(),
                    })
                })
                .or_else(|| match lookup_unit(&path, scope) {
                    UnitLookup::Declared(unit) => Some(unit),
                    UnitLookup::Ambiguous | UnitLookup::Unresolved => None,
                })?;
            Some(UnitShape {
                dimension_basis: declared.dimension_basis.clone(),
                scale: unit_fraction(&declared.scale)?,
                offset: unit_fraction(&declared.offset)?,
                affine: declared.affine,
            })
        }
        // Only linear units combine (ORNA-UNIT-005).
        Type::Applied { base, arguments } if matches!(base.as_str(), "*" | "/") => {
            let [left, right] = arguments.as_slice() else {
                return None;
            };
            let left = unit_shape(left, scope, home).filter(|shape| !shape.affine)?;
            let right = unit_shape(right, scope, home).filter(|shape| !shape.affine)?;
            let divide = base == "/";
            Some(UnitShape {
                dimension_basis: combine_dimension_bases(
                    &left.dimension_basis,
                    &right.dimension_basis,
                    divide,
                )?,
                scale: if divide {
                    left.scale.divide(&right.scale)?
                } else {
                    left.scale.multiply(&right.scale)?
                },
                offset: UnitFraction::zero(),
                affine: false,
            })
        }
        _ => None,
    }
}

/// Adds (or, when dividing, subtracts) dimension exponents, dropping the
/// dimensions that cancel.
pub(crate) fn combine_dimension_bases(
    left: &DimensionBasis,
    right: &DimensionBasis,
    divide: bool,
) -> Option<DimensionBasis> {
    let mut basis = left.clone();
    for (dimension, exponent) in right {
        let current = basis.entry(dimension.clone()).or_insert(0);
        *current = if divide {
            current.checked_sub(*exponent)?
        } else {
            current.checked_add(*exponent)?
        };
    }
    basis.retain(|_, exponent| *exponent != 0);
    Some(basis)
}

pub(crate) fn canonicalize_type_with_identities(
    ty: &Type,
    identities: &BTreeMap<String, String>,
) -> Type {
    match ty {
        Type::Named(name) => identities
            .get(name)
            .map(|identity| Type::Named(identity.clone()))
            .unwrap_or_else(|| ty.clone()),
        Type::List(inner) => Type::List(Box::new(canonicalize_type_with_identities(
            inner, identities,
        ))),
        Type::Range(inner) => Type::Range(Box::new(canonicalize_type_with_identities(
            inner, identities,
        ))),
        Type::Relation(inner) => Type::Relation(Box::new(canonicalize_type_with_identities(
            inner, identities,
        ))),
        Type::Stream(inner) => Type::Stream(Box::new(canonicalize_type_with_identities(
            inner, identities,
        ))),
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        canonicalize_type_with_identities(field, identities),
                    )
                })
                .collect(),
        ),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| canonicalize_type_with_identities(element, identities))
                .collect(),
        ),
        Type::Optional(inner) => Type::Optional(Box::new(canonicalize_type_with_identities(
            inner, identities,
        ))),
        Type::Applied { base, arguments } => Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| canonicalize_type_with_identities(argument, identities))
                .collect(),
        },
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            offset,
            affine,
        } => Type::Quantity {
            numeric: Box::new(canonicalize_type_with_identities(numeric, identities)),
            dimension_basis: dimension_basis.clone(),
            scale: scale.clone(),
            offset: offset.clone(),
            affine: *affine,
        },
        Type::MoneyPerUnit { currency, unit } => Type::MoneyPerUnit {
            currency: Box::new(canonicalize_type_with_identities(currency, identities)),
            unit: Box::new(canonicalize_type_with_identities(unit, identities)),
        },
        Type::Function {
            parameters,
            parameter_names,
            default_parameters,
            result,
        } => Type::Function {
            parameters: parameters
                .iter()
                .map(|parameter| canonicalize_type_with_identities(parameter, identities))
                .collect(),
            parameter_names: parameter_names.clone(),
            default_parameters: default_parameters.clone(),
            result: Box::new(canonicalize_type_with_identities(result, identities)),
        },
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Bottom
        | Type::Error => ty.clone(),
    }
}

pub(crate) fn is_nominal_symbol(symbol: &Symbol) -> bool {
    symbol.kind == SymbolKind::Type && symbol.table_schema.is_some()
}

/// Declarations whose source spellings canonicalise to a module-qualified
/// identity: nominal record types and tables.
pub(crate) fn has_qualified_identity(symbol: &Symbol) -> bool {
    is_nominal_symbol(symbol) || symbol.kind == SymbolKind::Table
}

pub(crate) fn canonical_nominal_name(name: &str, scope: &Scope) -> String {
    scope
        .nominal_identities
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_owned())
}

pub(crate) fn populate_nominal_identities(
    scope: &mut Scope,
    namespace: &Namespace,
    tree: &SyntaxTree,
) {
    for item in &tree.items {
        let name = match &item.declaration {
            Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { .. },
                ..
            }
            | Declaration::Table { name, .. } => name,
            _ => continue,
        };
        scope
            .nominal_identities
            .insert(name.clone(), nominal_identity(namespace, name));
    }
    let named_symbols = scope
        .names
        .iter()
        .filter_map(|(name, symbol)| {
            (has_qualified_identity(symbol))
                .then(|| match &symbol.ty {
                    Type::Named(identity) => Some((name.clone(), identity.clone())),
                    _ => None,
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    for (name, identity) in named_symbols {
        scope.nominal_identities.insert(name, identity);
    }
    let module_nominals = scope
        .available_modules
        .iter()
        .flat_map(|(module_namespace, module)| {
            module
                .exports
                .iter()
                .filter(|(_, symbol)| has_qualified_identity(symbol))
                .map(|(name, _)| {
                    (
                        module_namespace.clone(),
                        name.clone(),
                        nominal_identity(module_namespace, name),
                    )
                })
        })
        .collect::<Vec<_>>();
    for (module_namespace, name, identity) in module_nominals {
        scope
            .nominal_identities
            .insert(format!("{}.{}", module_namespace.display(), name), identity);
    }
    let aliases = scope
        .modules
        .iter()
        .map(|(alias, target)| (alias.clone(), target.clone()))
        .collect::<Vec<_>>();
    let aliased_nominals = aliases
        .into_iter()
        .flat_map(|(alias, target)| {
            let target_len = target.0.len();
            let target_prefix = target.0.clone();
            scope
                .available_modules
                .iter()
                .filter(move |(module_namespace, _)| {
                    module_namespace.0.len() >= target_len
                        && module_namespace.0[..target_len] == target_prefix[..]
                })
                .flat_map(move |(module_namespace, module)| {
                    let suffix = &module_namespace.0[target_len..];
                    module.exports.iter().filter_map({
                        let alias = alias.clone();
                        move |(name, symbol)| {
                            has_qualified_identity(symbol).then(|| {
                                let mut path = vec![alias.clone()];
                                path.extend(suffix.iter().cloned());
                                path.push(name.clone());
                                (path.join("."), nominal_identity(module_namespace, name))
                            })
                        }
                    })
                })
        })
        .collect::<Vec<_>>();
    for (path, identity) in aliased_nominals {
        scope.nominal_identities.insert(path, identity);
    }
}

/// Resolves the units of a signature in its declaring module, so an importer
/// never resolves another module's unit names in its own import context.
pub(crate) fn normalize_symbol_quantities(symbol: &mut Symbol, scope: &Scope) {
    if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Let) {
        symbol.ty = normalize_source_quantities(&symbol.ty, scope, None);
    }
}

pub(crate) fn canonicalize_symbol(symbol: &mut Symbol, identities: &BTreeMap<String, String>) {
    symbol.ty = canonicalize_type_with_identities(&symbol.ty, identities);
    if let Some(schema) = &mut symbol.table_schema {
        for field in schema.fields.values_mut() {
            *field = canonicalize_type_with_identities(field, identities);
        }
        if let Some(admission) = &mut schema.admission {
            for (_, field) in &mut admission.keys {
                *field = canonicalize_type_with_identities(field, identities);
            }
        }
        for (source, _) in &mut schema.conversions {
            *source = canonicalize_type_with_identities(source, identities);
        }
    }
}

pub(crate) fn canonicalize_scope_metadata(
    scope: &mut Scope,
    namespace: &Namespace,
    tree: &SyntaxTree,
) {
    populate_nominal_identities(scope, namespace, tree);
    let identities = scope.nominal_identities.clone();
    for module in scope.available_modules.values_mut() {
        for symbol in module.symbols.values_mut() {
            canonicalize_symbol(symbol, &identities);
        }
        for symbol in module.exports.values_mut() {
            canonicalize_symbol(symbol, &identities);
        }
    }
    for symbol in scope.names.values_mut() {
        canonicalize_symbol(symbol, &identities);
    }
    for row in scope.table_rows.values_mut() {
        *row = canonicalize_type_with_identities(row, &identities);
    }
    let mut nominal_rows = BTreeMap::new();
    for item in &tree.items {
        if let Some((name, row)) = nominal_row_type(item) {
            nominal_rows.insert(
                nominal_identity(namespace, &name),
                canonicalize_type_with_identities(&row, &identities),
            );
        }
    }
    for module in scope.available_modules.values() {
        for symbol in module.exports.values() {
            if !is_nominal_symbol(symbol) {
                continue;
            }
            if let Some(fields) = symbol.table_fields()
                && let Type::Named(identity) = &symbol.ty
            {
                nominal_rows.entry(identity.clone()).or_insert_with(|| {
                    canonicalize_type_with_identities(&Type::Record(fields.clone()), &identities)
                });
            }
        }
    }
    scope.nominal_rows = nominal_rows;
    let mut private_fields = BTreeMap::new();
    for item in &tree.items {
        let Declaration::Type {
            name,
            representation: TypeRepresentation::Nominal { members },
            ..
        } = &item.declaration
        else {
            continue;
        };
        let hidden = members
            .iter()
            .filter_map(|member| match member {
                TypeMember::Field {
                    visibility: false,
                    name,
                    ..
                } => Some(name.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if !hidden.is_empty() {
            private_fields.insert(nominal_identity(namespace, name), hidden);
        }
    }
    scope.nominal_private_fields = private_fields;
    for value in scope.refined_types.values_mut() {
        *value = canonicalize_type_with_identities(value, &identities);
    }
    for value in scope.type_aliases.values_mut() {
        *value = canonicalize_type_with_identities(value, &identities);
    }
    for variants in scope.enum_variants.values_mut() {
        for fields in variants.values_mut() {
            for value in fields.values_mut() {
                *value = canonicalize_type_with_identities(value, &identities);
            }
        }
    }
    normalize_scope_quantities(scope);
}

/// Resolves declared units in the types this module consumes: signatures,
/// table and nominal rows, refined bases and enum payloads become structural
/// quantities (ORNA-UNIT-001). A row keyed by its declaring module resolves
/// that module's units first. Table schemas keep their written units, which
/// schema descriptors identify by name.
pub(crate) fn normalize_scope_quantities(scope: &mut Scope) {
    let names = scope
        .names
        .iter()
        .filter(|(_, symbol)| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Let))
        .map(|(name, symbol)| {
            (
                name.clone(),
                normalize_source_quantities(&symbol.ty, scope, None),
            )
        })
        .collect::<Vec<_>>();
    for (name, ty) in names {
        if let Some(symbol) = scope.names.get_mut(&name) {
            symbol.ty = ty;
        }
    }
    let home = |key: &str| {
        key.rsplit_once('.')
            .map(|(namespace, _)| Namespace(namespace.split('.').map(str::to_owned).collect()))
    };
    let normalize_rows = |rows: &BTreeMap<String, Type>| {
        rows.iter()
            .map(|(key, row)| {
                let home = home(key);
                (
                    key.clone(),
                    normalize_source_quantities(row, scope, home.as_ref()),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let table_rows = normalize_rows(&scope.table_rows);
    let nominal_rows = normalize_rows(&scope.nominal_rows);
    let refined_types = normalize_rows(&scope.refined_types);
    let enum_variants = scope
        .enum_variants
        .iter()
        .map(|(name, variants)| {
            let variants = variants
                .iter()
                .map(|(variant, fields)| (variant.clone(), normalize_rows(fields)))
                .collect();
            (name.clone(), variants)
        })
        .collect();
    scope.table_rows = table_rows;
    scope.nominal_rows = nominal_rows;
    scope.refined_types = refined_types;
    scope.enum_variants = enum_variants;
}

pub(crate) fn primitive(name: &str) -> Option<Type> {
    Some(match name {
        "Int" => Type::Int,
        "Decimal" => Type::Decimal,
        "Float" => Type::Float,
        "Date" => Type::Date,
        "Instant" => Type::Instant,
        "Str" | "Text" | "String" => Type::Text,
        "Bool" => Type::Bool,
        "Null" => Type::Null,
        "Unit" => Type::Unit,
        // Annotated immutable content is a closed language type. Table fields
        // already resolve it; function signatures must resolve it too, because
        // the specified capture/insert helpers take and return `Blob`
        // (ORNA-BLOB-001, ORNA-BLOB-003).
        "Blob" => Type::Named("Blob".into()),
        "BOOLEAN" | "BOOL" => Type::Bool,
        "INTEGER" | "INT" | "BIGINT" => Type::Int,
        "FLOAT" => Type::Float,
        "DECIMAL" => Type::Decimal,
        "TEXT" => Type::Text,
        "DATE" => Type::Date,
        "TIMESTAMP" => Type::Instant,
        "Error" => Type::Named("Error".into()),
        "Duration" => Type::Named("std.DURATION".into()),
        "VOID" => Type::Null,
        "Uuid" => Type::Named("std.UUID".into()),
        "UUID" => Type::Named("std.UUID".into()),
        "TIME" => Type::Named("std.TIME".into()),
        "DURATION" => Type::Named("std.DURATION".into()),
        "BYTES" => Type::Named("std.BINARY_LARGE_OBJECT".into()),
        "Action" => Type::Named("std.Action".into()),
        "Rows" => Type::Named("std.Rows".into()),
        "JsonValue" => Type::Named("std.JsonValue".into()),
        "Document" => Type::Named("std.Document".into()),
        "ByteStream" => Type::Named("std.ByteStream".into()),
        "UI" => Type::Named("std.UI".into()),
        _ => return None,
    })
}
