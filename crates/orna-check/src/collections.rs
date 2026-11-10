//! Enum and finite-collection constructors.

use std::collections::{BTreeMap, BTreeSet};

use orna_intrinsics::IntrinsicId;
use orna_syntax::Expr;
use orna_value::Diagnostic;

use super::*;

/// Resolves the owner of `Enum.variant` or `module.Enum.variant`. An
/// unqualified and a qualified name follow the same construction rule, and
/// resolution must identify a constructible enum (lexical construction
/// rule; ORNA-ENUM-002/003). A local binding shadows an enum of the same
/// name, so field access on that value keeps ordinary record semantics.
pub(crate) fn enum_constructor<'a>(
    owner: &[&str],
    scope: &'a Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<EnumConstructor<'a>> {
    if local.contains_key(*owner.first()?) {
        return None;
    }
    let symbol = match owner {
        [name] => scope.names.get(*name),
        _ => qualified_module_symbol(owner, scope),
    }
    .filter(|symbol| symbol.kind == SymbolKind::Enum)?;
    let variants = match owner {
        [name] => scope.enum_variants.get(*name),
        _ => None,
    }
    .or_else(|| scope.imported_enum_variants.get(&owner.join(".")))?;
    Some(EnumConstructor {
        ty: symbol.ty.clone(),
        variants,
    })
}

/// ORNA-ENUM-002: `Enum.variant` references a payload-free variant. A
/// payload variant has no bare reference form (ORNA-ENUM-003 constructs it
/// with its named fields), so each omitted payload field is missing.
pub(crate) fn infer_enum_variant_reference(
    expr: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let path = qualified_path(expr)?;
    let (variant, owner) = path.split_last()?;
    let constructor = enum_constructor(owner, scope, local)?;
    let ty = match constructor.variants.get(*variant) {
        Some(payload) if payload.is_empty() => constructor.ty,
        Some(payload) => {
            for name in payload.keys() {
                diagnostics.push(missing_payload_field(name));
            }
            Type::Error
        }
        None => {
            diagnostics.push(unknown_enum_variant(owner, variant));
            Type::Error
        }
    };
    Some(Inferred::pure(ty))
}

/// ORNA-ENUM-003 and ORNA-SYNTAX-001: `Enum.variant { field: value }` checks
/// each supplied field once, in written order, against the declared payload
/// row. Unknown, duplicated, missing and mistyped fields are rejected, and a
/// payload-free variant accepts no field. Returns `None` when the path does
/// not name an enum, leaving nominal record construction unchanged.
pub(crate) fn infer_enum_payload_construction(
    path: &[orna_syntax::NameSegment],
    fields: &[orna_syntax::RecordField],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let path = path
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>();
    let (variant, owner) = path.split_last()?;
    let constructor = enum_constructor(owner, scope, local)?;
    let payload = constructor.variants.get(*variant);
    let mut valid = payload.is_some();
    if !valid {
        diagnostics.push(unknown_enum_variant(owner, variant));
    }
    let mut effects = EffectSummary::default();
    let mut supplied = BTreeSet::new();
    for field in fields {
        let expected = payload.and_then(|payload| payload.get(&field.name));
        let value = match expected {
            Some(expected) => infer_contextual(&field.value, expected, scope, local, diagnostics),
            None => infer(&field.value, scope, local, diagnostics),
        };
        effects.join(&value.effects);
        if !supplied.insert(field.name.as_str()) {
            diagnostics.push(diag(
                DIAG_DUPLICATE,
                format!(
                    "duplicate payload field `{}`",
                    diagnostic_field_name(&field.name)
                ),
            ));
            valid = false;
            continue;
        }
        match expected {
            Some(expected) if !types_match(expected, &value.ty) => {
                diagnostics.push(diag(
                    DIAG_FIELD_TYPE,
                    format!(
                        "expected {}, found {}",
                        diagnostic_type_name(expected),
                        diagnostic_type_name(&value.ty),
                    ),
                ));
                valid = false;
            }
            None if payload.is_some() => {
                diagnostics.push(diag(
                    DIAG_UNKNOWN_FIELD,
                    format!("unknown field `{}`", diagnostic_field_name(&field.name)),
                ));
                valid = false;
            }
            _ => {}
        }
    }
    for name in payload
        .into_iter()
        .flat_map(BTreeMap::keys)
        .filter(|name| !supplied.contains(name.as_str()))
    {
        diagnostics.push(missing_payload_field(name));
        valid = false;
    }
    Some(Inferred {
        ty: if valid { constructor.ty } else { Type::Error },
        effects,
    })
}

pub(crate) fn missing_payload_field(name: &str) -> Diagnostic {
    diag(
        DIAG_MISSING_FIELD,
        format!("missing required field `{}`", diagnostic_field_name(name)),
    )
}

/// ORNA-ENUM-001: an enum is closed to the variants declared in its body.
pub(crate) fn unknown_enum_variant(owner: &[&str], variant: &str) -> Diagnostic {
    let owner = owner.join(".");
    diag(
        DIAG_UNRESOLVED,
        if diagnostic_identifier(&owner) && diagnostic_identifier(variant) {
            format!("enum `{owner}` has no variant `{variant}`")
        } else {
            "enum variant cannot be resolved".to_owned()
        },
    )
}

pub(crate) fn diagnostic_field_name(name: &str) -> &str {
    if diagnostic_identifier(name) {
        name
    } else {
        "<field>"
    }
}

/// Resolves the finite, declarative core stream constructor. The result is a
/// type/effect summary; list hashing, replay and checkpoint behavior belong to
/// the runtime contract.
pub(crate) fn infer_stream_from_list(
    callee: &Expr,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if qualified_path(callee)?.as_slice() != ["Stream", "from_list"] {
        return None;
    }
    let values = arguments.first().map(|argument| &argument.value);
    let source_identity = arguments.get(1);
    let Some(values) = values else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "Stream.from_list requires values and source_identity",
        ));
        return Some(Inferred::error());
    };
    let values = infer(values, scope, local, diagnostics);
    let valid_identity = source_identity.is_some_and(|argument| {
        argument.name.as_deref() == Some("source_identity")
            && matches!(
                infer(&argument.value, scope, local, diagnostics).ty,
                Type::Text
            )
    });
    if arguments.len() != 2 || !valid_identity {
        diagnostics.push(diag(
            DIAG_TYPE,
            "Stream.from_list requires a named Text source_identity",
        ));
    }
    let Type::List(element) = values.ty else {
        diagnostics.push(diag(DIAG_TYPE, "Stream.from_list values must be a list"));
        return Some(Inferred::error_with(values.effects));
    };
    Some(Inferred {
        ty: Type::Stream(element),
        effects: values.effects,
    })
}

/// The element type a pinned aggregate declaration supplies for an empty-list
/// contextual check (`sum`, `min`, `max`). Generic declarations have no single
/// instantiating type, so they return `None` and the ordinary rule applies.
pub(crate) fn finite_list_aggregate_element(
    callee: &Expr,
    operation: &str,
    scope: &Scope,
) -> Option<Type> {
    if !matches!(operation, "sum" | "min" | "max") {
        return None;
    }
    let namespace = match qualified_path(callee)?.as_slice() {
        ["std", module @ ("collection" | "query"), _] => {
            Namespace(vec!["std".into(), (*module).into()])
        }
        [alias, _] => scope.modules.get(*alias)?.clone(),
        _ => return None,
    };
    if !(namespace.0.len() == 2
        && namespace.0[0] == "std"
        && matches!(namespace.0[1].as_str(), "collection" | "query"))
    {
        return None;
    }
    let symbol = scope
        .available_modules
        .get(&namespace)?
        .exports
        .get(operation)?;
    let Type::Function {
        parameters, result, ..
    } = &symbol.ty
    else {
        return None;
    };
    let [Type::List(element)] = parameters.as_slice() else {
        return None;
    };
    let pinned_result = if operation == "sum" {
        result.as_ref() == element.as_ref()
    } else {
        matches!(result.as_ref(), Type::Optional(inner) if inner == element)
    };
    (symbol.generic_parameters.is_empty() && pinned_result).then(|| element.as_ref().clone())
}

/// Whether a pipeline stage is a finite-list numeric aggregate, whose rows
/// argument is inferred with an empty-list element context.
pub(crate) fn is_finite_list_numeric_aggregate_pipeline_stage(
    expression: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> bool {
    let callee = match expression {
        Expr::Call { callee, .. } => callee.as_ref(),
        Expr::Name { .. } | Expr::Field { .. } => expression,
        _ => return false,
    };
    resolve_intrinsic(callee, scope, local).is_some_and(|resolved| {
        matches!(
            resolved.id,
            IntrinsicId::CoreSum | IntrinsicId::CoreMin | IntrinsicId::CoreMax
        )
    })
}
