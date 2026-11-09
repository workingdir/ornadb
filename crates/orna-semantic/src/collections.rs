//! Enum and finite-collection constructors.

use std::collections::{BTreeMap, BTreeSet};

use orna_foundation::Diagnostic;
use orna_syntax::Expr;

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

/// Only a public export from the captured std.collection/std.query modules
/// admits this intrinsic. The query declaration is a source-level re-export
/// of the same operation identity, not an additional host implementation.
pub(crate) fn standard_collection_operation_is_admitted(operation: &str, scope: &Scope) -> bool {
    ["collection", "query"].into_iter().any(|module_name| {
        scope
            .available_modules
            .get(&Namespace(vec!["std".into(), module_name.into()]))
            .is_some_and(|module| {
                module
                    .exports
                    .get(operation)
                    .is_some_and(|symbol| symbol.kind == SymbolKind::Function)
            })
    })
}

pub(crate) fn is_portable_collection_operation(operation: &str) -> bool {
    matches!(
        operation,
        "filter"
            | "map"
            | "flat_map"
            | "sort_by"
            | "chunk"
            | "flatten"
            | "partition"
            | "zip"
            | "zip_exact"
            | "unique"
            | "group_by"
            | "pairs"
            | "window"
            | "split_when"
            | "rank"
            | "asof_join"
            | "bucket_by"
            | "take"
            | "drop"
            | "distinct"
            | "union"
            | "count"
            | "first"
            | "one"
            | "sum"
            | "min"
            | "max"
            | "every"
            | "exists"
    )
}

pub(crate) fn standard_collection_module_operation<'a>(
    callee: &'a Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<&'a str> {
    let path = qualified_path(callee)?;
    match path.as_slice() {
        ["std", "collection" | "query", operation]
            if !local.contains_key("std")
                && is_portable_collection_operation(operation)
                && standard_collection_operation_is_admitted(operation, scope) =>
        {
            Some(*operation)
        }
        [alias, operation] if !local.contains_key(*alias) => {
            let namespace = scope.modules.get(*alias)?;
            if namespace.0.len() == 2
                && namespace.0[0] == "std"
                && matches!(namespace.0[1].as_str(), "collection" | "query")
                && is_portable_collection_operation(operation)
                && scope
                    .available_modules
                    .get(namespace)
                    .is_some_and(|module| {
                        module
                            .exports
                            .get(*operation)
                            .is_some_and(|symbol| symbol.kind == SymbolKind::Function)
                    })
            {
                Some(*operation)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(crate) fn standard_statistics_module_operation<'a>(
    callee: &'a Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<&'a str> {
    let path = qualified_path(callee)?;
    match path.as_slice() {
        ["std", "stats", operation]
            if !local.contains_key("std") && is_portable_statistics_operation(operation) =>
        {
            Some(*operation)
        }
        [alias, operation] if !local.contains_key(*alias) => {
            let namespace = scope.modules.get(*alias)?;
            if namespace.0.len() == 2
                && namespace.0[0] == "std"
                && namespace.0[1] == "stats"
                && is_portable_statistics_operation(operation)
                && scope
                    .available_modules
                    .get(namespace)
                    .is_some_and(|module| {
                        module
                            .exports
                            .get(*operation)
                            .is_some_and(|symbol| symbol.kind == SymbolKind::Function)
                    })
            {
                Some(*operation)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(crate) fn is_portable_statistics_operation(operation: &str) -> bool {
    matches!(
        operation,
        "mean"
            | "median"
            | "percentile"
            | "sum"
            | "min"
            | "max"
            | "range"
            | "mode"
            | "variance"
            | "standard_deviation"
            | "histogram"
            | "rate"
            | "derivative"
            | "integrate"
    )
}

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

/// Root collection names are ordinary, shadowable bindings in 1.0.0. Keep
/// the closed intrinsic overloads behind the same lexical lookup rule used by
/// `infer(Name)`: a local binding or source/imported symbol wins first.
pub(crate) fn root_collection_intrinsic_is_unshadowed(
    operation: &str,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> bool {
    matches!(
        operation,
        "every"
            | "exists"
            | "first"
            | "one"
            | "map"
            | "flat_map"
            | "sort_by"
            | "filter"
            | "distinct"
            | "take"
            | "drop"
            | "window"
            | "count"
            | "union"
            | "sum"
            | "min"
            | "max"
    ) && !local.contains_key(operation)
        && !scope.names.contains_key(operation)
}

pub(crate) fn finite_list_collection_operation<'a>(
    callee: &'a Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<&'a str> {
    let path = qualified_path(callee)?;
    if path.len() == 1
        && matches!(
            path[0],
            "every"
                | "exists"
                | "first"
                | "one"
                | "map"
                | "flat_map"
                | "sort_by"
                | "sum"
                | "min"
                | "max"
        )
        && root_collection_intrinsic_is_unshadowed(path[0], scope, local)
    {
        return path.last().copied();
    }
    standard_collection_module_operation(callee, scope, local)
}

pub(crate) fn is_finite_list_numeric_aggregate_pipeline_stage(
    expression: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> bool {
    let operation = match expression {
        Expr::Call { callee, .. } => finite_list_collection_operation(callee, scope, local),
        Expr::Name { .. } | Expr::Field { .. } => {
            finite_list_collection_operation(expression, scope, local)
        }
        _ => None,
    };
    matches!(operation, Some("sum" | "min" | "max"))
}

pub(crate) fn infer_finite_list_collection_call(
    callee: &Expr,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let path = qualified_path(callee)?;
    let portable_collection_call = match path.as_slice() {
        [operation] => matches!(
            *operation,
            "filter"
                | "map"
                | "flat_map"
                | "sort_by"
                | "take"
                | "drop"
                | "distinct"
                | "union"
                | "count"
                | "first"
                | "one"
                | "sum"
                | "min"
                | "max"
                | "every"
                | "exists"
                | "chunk"
                | "flatten"
                | "partition"
                | "zip"
                | "zip_exact"
                | "unique"
                | "group_by"
                | "pairs"
                | "window"
                | "split_when"
                | "rank"
                | "asof_join"
                | "bucket_by"
        ),
        _ => standard_collection_module_operation(callee, scope, local).is_some(),
    };
    if portable_collection_call
        && let Some(argument) = arguments
            .iter()
            .find(|argument| argument.name.as_deref() == Some("rows"))
            .or_else(|| {
                arguments
                    .iter()
                    .find(|argument| argument.name.as_deref() == Some("values"))
            })
            .or_else(|| {
                arguments
                    .iter()
                    .find(|argument| argument.name.as_deref() == Some("left"))
            })
            .or_else(|| arguments.first())
    {
        let mut probe_diagnostics = Vec::new();
        let inferred = infer(&argument.value, scope, local, &mut probe_diagnostics);
        if matches!(inferred.ty, Type::Relation(_)) {
            let operation = standard_collection_module_operation(callee, scope, local)
                .or_else(|| path.first().copied());
            if !operation.is_some_and(|operation| {
                matches!(
                    operation,
                    "chunk"
                        | "flatten"
                        | "partition"
                        | "zip"
                        | "zip_exact"
                        | "unique"
                        | "group_by"
                        | "pairs"
                        | "window"
                        | "split_when"
                        | "rank"
                        | "asof_join"
                        | "bucket_by"
                )
            }) {
                return None;
            }
        }
    }
    let operation = match path.as_slice() {
        ["every"] => "every",
        ["exists"] => "exists",
        ["first"] => "first",
        ["one"] => "one",
        ["sum"] => "sum",
        ["min"] => "min",
        ["max"] => "max",
        ["sort_by"] if root_collection_intrinsic_is_unshadowed("sort_by", scope, local) => {
            "sort_by"
        }
        [operation]
            if matches!(
                *operation,
                "filter" | "map" | "flat_map" | "take" | "drop" | "distinct" | "union" | "count"
            ) && root_collection_intrinsic_is_unshadowed(operation, scope, local) =>
        {
            *operation
        }
        _ => standard_collection_module_operation(callee, scope, local)?,
    };
    Some(infer_finite_list_collection(
        operation,
        None,
        arguments,
        finite_list_aggregate_element(callee, operation, scope),
        scope,
        local,
        diagnostics,
    ))
}

/// Checks the finite-list overload of the standard collection mapper. A
/// pipeline supplies `rows` as its implicit first argument; direct calls use
/// the same `rows`/`transform` names and ordinary positional insertion.
pub(crate) fn infer_finite_list_collection(
    operation: &str,
    input: Option<Inferred>,
    arguments: &[orna_syntax::Argument],
    aggregate_element: Option<Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if matches!(
        operation,
        "chunk"
            | "flatten"
            | "partition"
            | "zip"
            | "zip_exact"
            | "unique"
            | "group_by"
            | "pairs"
            | "window"
            | "split_when"
            | "rank"
            | "asof_join"
            | "bucket_by"
    ) {
        return infer_finite_list_helper_collection(
            operation,
            input,
            arguments,
            scope,
            local,
            diagnostics,
        );
    }
    if operation == "first" {
        return infer_finite_list_first(arguments, input, scope, local, diagnostics);
    }
    if operation == "one" {
        return infer_finite_list_one(arguments, input, scope, local, diagnostics);
    }
    if matches!(operation, "sum" | "min" | "max") {
        return infer_finite_list_numeric_aggregate(
            operation,
            arguments,
            input,
            aggregate_element,
            scope,
            local,
            diagnostics,
        );
    }
    if operation == "sort_by" {
        return infer_finite_list_sort_by(arguments, input, scope, local, diagnostics);
    }
    if matches!(operation, "every" | "exists") {
        return infer_finite_list_every_exists(
            operation,
            arguments,
            input,
            scope,
            local,
            diagnostics,
        );
    }
    if matches!(operation, "distinct" | "count" | "take" | "drop" | "union") {
        return infer_finite_list_simple_collection(
            operation,
            input,
            arguments,
            scope,
            local,
            diagnostics,
        );
    }
    let pipeline = input.is_some();
    let mut slots = [None, None];
    let mut malformed = false;
    let mut positional = usize::from(pipeline);
    let mut named_started = false;
    for (index, argument) in arguments.iter().enumerate() {
        let slot = match argument.name.as_deref() {
            Some("rows") => {
                named_started = true;
                Some(0)
            }
            Some("predicate") if operation == "filter" => {
                named_started = true;
                Some(1)
            }
            Some("transform") if matches!(operation, "map" | "flat_map") => {
                named_started = true;
                Some(1)
            }
            Some(_) => {
                malformed = true;
                None
            }
            None if named_started || positional >= slots.len() => {
                malformed = true;
                None
            }
            None => {
                let slot = positional;
                positional += 1;
                Some(slot)
            }
        };
        if let Some(slot) = slot
            && (pipeline && slot == 0 || slots[slot].replace(index).is_some())
        {
            malformed = true;
        }
    }
    if pipeline {
        if slots[0].is_some() {
            malformed = true;
        }
    } else if slots[0].is_none() {
        malformed = true;
    }
    if slots[1].is_none() {
        malformed = true;
    }

    let mut effects = input
        .as_ref()
        .map(|input| input.effects.clone())
        .unwrap_or_default();
    let row = if let Some(input) = input {
        input.ty
    } else if let Some(index) = slots[0] {
        let row = infer(&arguments[index].value, scope, local, diagnostics);
        effects.join(&row.effects);
        row.ty
    } else {
        for argument in arguments {
            if malformed && matches!(argument.value, Expr::Lambda { .. }) {
                continue;
            }
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
        }
        Type::Error
    };
    let callback = slots[1].map(|index| {
        let element = match &row {
            Type::List(element) => element.as_ref().clone(),
            _ => Type::Error,
        };
        let callback = infer_finite_list_callback(
            &arguments[index].value,
            element,
            if operation == "filter" {
                Type::Bool
            } else {
                Type::Error
            },
            scope,
            local,
            diagnostics,
        );
        effects.join(&callback.effects);
        callback
    });

    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("std.collection {operation} arguments do not match its static signature"),
        ));
    }
    if !matches!(&row, Type::List(_)) {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("std.collection {operation} requires a finite list"),
        ));
        return Inferred::error_with(effects);
    }
    let Some(callback) = callback else {
        return Inferred::error_with(effects);
    };
    let ty = match operation {
        "filter" => {
            if callback.ty == Type::Bool {
                let Type::List(element) = row else {
                    unreachable!("finite-list row type was checked")
                };
                Type::List(element)
            } else {
                Type::Error
            }
        }
        "map" => Type::List(Box::new(callback.ty)),
        "flat_map" => match callback.ty {
            Type::List(element) => Type::List(element),
            Type::Error => Type::Error,
            _ => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "std.collection flat_map transform must return a finite list",
                ));
                Type::Error
            }
        },
        _ => unreachable!("finite list collection operation was checked"),
    };
    Inferred { ty, effects }
}

/// Static signatures for the remaining finite std.collection/std.query
/// helpers. Their runtime bodies remain evaluator intrinsics admitted by the
/// same captured module exports.
pub(crate) fn infer_finite_list_helper_collection(
    operation: &str,
    input: Option<Inferred>,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let expected: &[&str] = match operation {
        "chunk" => &["values", "size"],
        "flatten" | "unique" | "pairs" => &["values"],
        "partition" | "split_when" => &["values", "predicate"],
        "zip" | "zip_exact" => &["left", "right"],
        "group_by" | "rank" => &["values", "key"],
        "window" => &["values", "size", "step"],
        "asof_join" => &["left", "right", "time", "by"],
        "bucket_by" => &["rows", "period", "zone"],
        _ => unreachable!("finite-list helper was checked"),
    };
    let pipeline = input.is_some();
    let mut slots = vec![None; expected.len()];
    let mut values = vec![None; expected.len()];
    let mut malformed = false;
    let mut positional = usize::from(pipeline);
    let mut named_started = false;
    let mut effects = input
        .as_ref()
        .map(|input| input.effects.clone())
        .unwrap_or_default();
    if let Some(input) = input {
        values[0] = Some(input.ty);
    }
    for (index, argument) in arguments.iter().enumerate() {
        let slot = if let Some(name) = argument.name.as_deref() {
            named_started = true;
            expected.iter().position(|expected| *expected == name)
        } else if named_started || positional >= expected.len() {
            None
        } else {
            let slot = positional;
            positional += 1;
            Some(slot)
        };
        let contextual_callback = slot.is_some_and(|slot| {
            matches!(
                (operation, slot),
                ("partition" | "split_when", 1) | ("group_by" | "rank", 1) | ("asof_join", 2 | 3)
            )
        }) && matches!(&argument.value, Expr::Lambda { .. });
        let inferred = if contextual_callback {
            Inferred::error()
        } else {
            infer(&argument.value, scope, local, diagnostics)
        };
        effects.join(&inferred.effects);
        if let Some(slot) = slot {
            if (pipeline && slot == 0) || slots[slot].replace(index).is_some() {
                malformed = true;
            } else {
                values[slot] = Some(inferred.ty);
            }
        } else {
            malformed = true;
        }
    }
    let required = if operation == "window" {
        expected.len() - 1
    } else {
        expected.len()
    };
    if values.iter().take(required).any(Option::is_none) {
        malformed = true;
    }
    if operation == "window" && values[2].is_none() {
        values[2] = Some(Type::Int);
    }
    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("std.collection {operation} arguments do not match its static signature"),
        ));
    }

    let call = FiniteListCall {
        operation,
        values: &values,
        slots: &slots,
        arguments,
        scope,
        local,
    };
    let result = finite_list_helper_result(call, &mut effects, diagnostics);
    Inferred {
        ty: if malformed { Type::Error } else { result },
        effects,
    }
}
