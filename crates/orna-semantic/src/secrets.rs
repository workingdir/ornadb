//! Secret graph and type identity.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    Declaration, Pattern, ProtocolMember, SyntaxTree, TypeExpr, TypeMember, TypeRepresentation,
    UseTail,
};
use orna_value::Diagnostic;

use super::*;

/// Adds the nominal rows and transparent aliases of `tree` to the secret graph, keyed by
/// canonical identity. Member and target names are rewritten through `spellings`, so a
/// reference to a type of another module is followed to that type's definition. Private
/// members are included: they are committed with the record.
pub(crate) fn secret_definitions(
    namespace: &Namespace,
    tree: &SyntaxTree,
    spellings: &BTreeMap<String, String>,
    rows: &mut BTreeMap<String, Type>,
    aliases: &mut BTreeMap<String, Type>,
) {
    for item in &tree.items {
        match &item.declaration {
            Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { .. },
                ..
            } => {
                if let Some((_, row)) = nominal_row_type(item) {
                    rows.insert(
                        nominal_identity(namespace, name),
                        canonicalize_type_with_identities(&row, spellings),
                    );
                }
            }
            Declaration::Type {
                name,
                generics,
                representation: TypeRepresentation::Alias { ty, refinements },
            } if generics.is_empty() && refinements.is_empty() => {
                aliases.insert(
                    nominal_identity(namespace, name),
                    canonicalize_type_with_identities(&type_of(ty), spellings),
                );
            }
            _ => {}
        }
    }
}

/// Names through which a module reaches a type whose definition holds a secret
/// (ORNA-SECRET-001): the canonical identity and the module's own spelling of it. Locally
/// declared names are excluded: their secrecy is checked from the local definitions.
pub(crate) fn imported_secret_types(
    tree: &SyntaxTree,
    spellings: &BTreeMap<String, String>,
    rows: &BTreeMap<String, Type>,
    aliases: &BTreeMap<String, Type>,
) -> BTreeSet<String> {
    let declared: BTreeSet<&str> = tree
        .items
        .iter()
        .filter_map(|item| match &item.declaration {
            Declaration::Type { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let mut secret = BTreeSet::new();
    for (spelling, identity) in spellings {
        if declared.contains(spelling.as_str()) {
            continue;
        }
        if contains_secret_value_type_in(
            &Type::Named(identity.clone()),
            rows,
            aliases,
            &BTreeSet::new(),
            &mut BTreeSet::new(),
        ) {
            secret.insert(identity.clone());
            secret.insert(spelling.clone());
        }
    }
    secret
}

/// Canonical identities of the nominal types in scope that implement the core
/// `Currency` protocol (ORNA-MONEY-004): this module's declarations whose
/// nested `impl Currency` names it, and the imported or attached nominal types
/// exported as currencies. Nothing is inferred from a name.
pub(crate) fn currency_types(
    namespace: &Namespace,
    tree: &SyntaxTree,
    scope: &Scope,
) -> BTreeSet<String> {
    let mut currencies = BTreeSet::new();
    if currency_is_core_protocol(scope) {
        currencies.extend(tree.items.iter().filter_map(|item| {
            let Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { members },
                ..
            } = &item.declaration
            else {
                return None;
            };
            members
                .iter()
                .any(|member| {
                    matches!(
                        member,
                        TypeMember::Implementation { implementation, .. }
                            if names_currency_protocol(&implementation.protocol)
                    )
                })
                .then(|| nominal_identity(namespace, name))
        }));
    }
    let exported = scope.names.values().chain(
        scope
            .available_modules
            .values()
            .flat_map(|module| module.exports.values()),
    );
    currencies.extend(
        exported.filter_map(|symbol| match (&symbol.ty, &symbol.table_schema) {
            (Type::Named(identity), Some(schema))
                if symbol.kind == SymbolKind::Type && schema.currency =>
            {
                Some(identity.clone())
            }
            _ => None,
        }),
    );
    currencies
}

/// Whether a protocol reference is the unqualified, argument-free `Currency`.
pub(crate) fn names_currency_protocol(protocol: &TypeExpr) -> bool {
    matches!(
        protocol,
        TypeExpr::Name { path, arguments, .. }
            if path.as_slice() == ["Currency"] && arguments.is_empty()
    )
}

/// Whether `Currency` names the core protocol in this scope. A declared or
/// imported protocol named `Currency` takes precedence (ORNA-IMPORT-007), and
/// it is the core protocol only when its member surface is exactly the core
/// surface.
pub(crate) fn currency_is_core_protocol(scope: &Scope) -> bool {
    match resolve_local_protocol_name("Currency", scope) {
        Some(protocol) => scope.local_protocols.get(&protocol).is_some_and(|members| {
            static_protocol_surface(members)
                == static_protocol_surface(CORE_CURRENCY_SURFACE.as_slice())
        }),
        None => !scope.names.contains_key("Currency"),
    }
}

/// A protocol's static properties by name and type, or `None` when it also
/// declares functions. Spans and member order are not part of the surface.
pub(crate) fn static_protocol_surface(members: &[ProtocolMember]) -> Option<BTreeMap<&str, Type>> {
    members
        .iter()
        .map(|member| match member {
            ProtocolMember::Static { name, ty, .. } => Some((name.as_str(), type_of(ty))),
            ProtocolMember::Function { .. } => None,
        })
        .collect()
}

/// Whether `ty` can parameterize `Money`: a nominal type that implements the
/// core `Currency` protocol, or a generic parameter bounded by it.
pub(crate) fn is_currency_type(ty: &Type, scope: &Scope) -> bool {
    let Type::Named(identity) = ty else {
        return false;
    };
    scope.currency_types.contains(identity)
        || (scope.generic_type_parameters.contains(identity)
            && scope
                .generic_type_bounds
                .get(identity)
                .is_some_and(|bounds| bounds.iter().any(names_currency_protocol))
            && currency_is_core_protocol(scope))
}

pub(crate) fn enum_variant_types(
    tree: &SyntaxTree,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<String, BTreeMap<String, BTreeMap<String, Type>>> {
    let mut enums = BTreeMap::new();
    for item in &tree.items {
        let Declaration::Enum { name, variants, .. } = &item.declaration else {
            continue;
        };
        let mut typed_variants = BTreeMap::new();
        for variant in variants {
            let mut fields = BTreeMap::new();
            for field in &variant.fields {
                if fields
                    .insert(field.name.clone(), type_of(&field.ty))
                    .is_some()
                {
                    diagnostics.push(diag(DIAG_DUPLICATE, "duplicate enum payload field"));
                }
            }
            if typed_variants
                .insert(variant.name.clone(), fields)
                .is_some()
            {
                diagnostics.push(diag(DIAG_DUPLICATE, "duplicate enum variant"));
            }
        }
        enums.insert(name.clone(), typed_variants);
    }
    enums
}

pub(crate) fn payload_enum_names(tree: &SyntaxTree) -> BTreeSet<String> {
    tree.items
        .iter()
        .filter_map(|item| match &item.declaration {
            Declaration::Enum { name, variants, .. }
                if variants.iter().any(|variant| !variant.fields.is_empty()) =>
            {
                Some(name.clone())
            }
            _ => None,
        })
        .collect()
}

/// Declared payload rows of every enum in one source module. Payload field
/// types name that module's own nominal records by their module identity, so
/// an importer checks construction against the same types the declaring
/// module uses. Duplicate declarations are reported by the declaring module.
pub(crate) fn source_enum_variant_rows(
    namespace: &Namespace,
    tree: &SyntaxTree,
) -> BTreeMap<String, EnumVariantRows> {
    let identities = tree
        .items
        .iter()
        .filter_map(|item| match &item.declaration {
            Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { .. },
                ..
            } => Some((name.clone(), nominal_identity(namespace, name))),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut enums = enum_variant_types(tree, &mut Vec::new());
    for field in enums
        .values_mut()
        .flat_map(BTreeMap::values_mut)
        .flat_map(BTreeMap::values_mut)
    {
        *field = canonicalize_type_with_identities(field, &identities);
    }
    enums
}

/// Payload rows of the source enums each `use` declaration makes visible,
/// keyed by the spelling that names the enum in this module: `Level` for a
/// named, glob or prelude import, and `alias.Level` through a module binding.
pub(crate) fn imported_enum_bindings(
    tree: &SyntaxTree,
    source_enums: &BTreeMap<Namespace, BTreeMap<String, EnumVariantRows>>,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) -> BTreeMap<String, EnumVariantRows> {
    let mut bindings = BTreeMap::new();
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let target = Namespace(path.iter().map(|segment| segment.name.clone()).collect());
        let Some(module) = modules.get(&target) else {
            continue;
        };
        let enums = source_enums.get(&target).into_iter().flatten();
        match tail {
            UseTail::None => {
                if let Some(binding) = target.0.last() {
                    add_qualified_enum_bindings(
                        &mut bindings,
                        binding,
                        &target,
                        source_enums,
                        modules,
                    );
                }
            }
            UseTail::Alias { name, .. } if name != "_" => {
                add_qualified_enum_bindings(&mut bindings, name, &target, source_enums, modules);
            }
            UseTail::Alias { .. } => bindings.extend(
                enums
                    .filter(|(name, _)| {
                        module.prelude_exports.contains(*name) && is_exported_enum(module, name)
                    })
                    .map(|(name, variants)| (name.clone(), variants.clone())),
            ),
            UseTail::Glob { .. } => bindings.extend(
                enums
                    .filter(|(name, _)| is_exported_enum(module, name))
                    .map(|(name, variants)| (name.clone(), variants.clone())),
            ),
            UseTail::Names(imports) => bindings.extend(imports.iter().filter_map(|import| {
                let variants = source_enums.get(&target)?.get(&import.name)?;
                is_exported_enum(module, &import.name)
                    .then(|| (import.name.clone(), variants.clone()))
            })),
        }
    }
    bindings
}

pub(crate) fn add_qualified_enum_bindings(
    bindings: &mut BTreeMap<String, EnumVariantRows>,
    binding: &str,
    target: &Namespace,
    source_enums: &BTreeMap<Namespace, BTreeMap<String, EnumVariantRows>>,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) {
    for (namespace, enums) in source_enums {
        if namespace.0.len() < target.0.len() || namespace.0[..target.0.len()] != target.0[..] {
            continue;
        }
        let Some(module) = modules.get(namespace) else {
            continue;
        };
        let mut prefix = vec![binding.to_owned()];
        prefix.extend(namespace.0[target.0.len()..].iter().cloned());
        for (name, variants) in enums {
            if is_exported_enum(module, name) {
                let mut qualified = prefix.clone();
                qualified.push(name.clone());
                bindings.insert(qualified.join("."), variants.clone());
            }
        }
    }
}

pub(crate) fn is_exported_enum(module: &ModuleHeader, name: &str) -> bool {
    module
        .exports
        .get(name)
        .is_some_and(|symbol| symbol.kind == SymbolKind::Enum)
}

pub(crate) fn pattern_binding_names(pattern: &Pattern) -> Vec<String> {
    fn collect(pattern: &Pattern, names: &mut Vec<String>) {
        match pattern {
            Pattern::Name(name, _) => names.push(name.clone()),
            Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
                for element in elements {
                    collect(element, names);
                }
            }
            Pattern::Record { fields, .. } => {
                for (name, pattern, _) in fields {
                    if let Some(pattern) = pattern {
                        collect(pattern, names);
                    } else {
                        names.push(name.clone());
                    }
                }
            }
            Pattern::Constructor {
                arguments, fields, ..
            } => {
                for argument in arguments {
                    collect(argument, names);
                }
                for field in fields {
                    if let Some(pattern) = &field.pattern {
                        collect(pattern, names);
                    } else {
                        names.push(field.name.clone());
                    }
                }
            }
            Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
        }
    }

    let mut names = Vec::new();
    collect(pattern, &mut names);
    names
}

pub(crate) fn resolved_helper_summary_key(
    path: &[&str],
    local_names: &BTreeSet<String>,
    scope: &Scope,
) -> Option<String> {
    let first = *path.first()?;
    if local_names.contains(first) {
        return None;
    }
    if path.len() == 1 {
        return scope
            .names
            .get(first)
            .filter(|symbol| symbol.kind == SymbolKind::Function)
            .map(|_| first.to_owned());
    }
    let function = qualified_module_symbol(path, scope)?;
    (function.kind == SymbolKind::Function).then(|| path.join("."))
}
