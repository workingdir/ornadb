//! Pipeline, generic and intrinsic call inference.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax_ast::{Expr, LiteralKind, Pattern, TypeExpr};
use orna_value::{Diagnostic, SafeText};

use super::*;

/// Checks the operation's own constraints on the relation element and bound
/// arguments, and types the successful result.
pub(crate) fn statistics_outcome(
    call: StatisticsOutcome<'_>,
    element: Type,
    valid: &mut bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    let StatisticsOutcome {
        operation,
        parameters,
        values,
        scope,
    } = call;
    let argument_type = |slot: usize| {
        values
            .get(slot)
            .and_then(Option::as_ref)
            .map(|value| &value.ty)
    };
    let numeric = is_statistics_numeric_type(&element);
    match operation {
        "mean" | "median" | "variance" | "standard_deviation" | "percentile" if !numeric => {
            diagnostics.push(diag(
                DIAG_TYPE,
                format!("std.stats.{operation} requires numeric relation rows"),
            ));
            *valid = false;
        }
        "sum" if is_absolute_affine(&element) => {
            diagnostics.push(diag(
                DIAG_ABSOLUTE_QUANTITY_SUM,
                "cannot sum absolute affine quantities",
            ));
            *valid = false;
        }
        "sum" if !is_sum_element_type(&element) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "std.stats.sum requires numeric relation rows",
            ));
            *valid = false;
        }
        "min" | "max" if !is_sort_key_type(&element) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                format!("std.stats.{operation} requires ordered relation rows"),
            ));
            *valid = false;
        }
        "range" if !numeric => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "std.stats.range requires numeric relation rows",
            ));
            *valid = false;
        }
        "mode" if is_default_float_equality_type(&element) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "std.stats.mode requires a lawful equality relation; default Float equality is unavailable",
            ));
            *valid = false;
        }
        "histogram" if !is_sort_key_type(&element) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "std.stats.histogram requires ordered relation rows",
            ));
            *valid = false;
        }
        "rate" | "derivative" | "integrate" => match &element {
            Type::Tuple(parts)
                if parts.len() == 2
                    && parts[0] == Type::Instant
                    && is_statistics_numeric_type(&parts[1]) => {}
            _ => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("std.stats.{operation} requires (Instant, numeric) relation rows"),
                ));
                *valid = false;
            }
        },
        _ => {}
    }

    let numeric_argument_slots = match operation {
        "percentile" => &[1][..],
        _ => &[][..],
    };
    for slot in numeric_argument_slots {
        if let Some(ty) = argument_type(*slot)
            && *ty != Type::Error
            && !is_statistics_numeric_type(ty)
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "std.stats.percentile probability must be numeric",
            ));
            *valid = false;
        }
    }
    if operation == "percentile"
        && let Some(ty) = argument_type(2)
        && !matches!(ty, Type::Text | Type::Error)
    {
        diagnostics.push(diag(
            DIAG_TYPE,
            "std.stats.percentile interpolation must be Str",
        ));
        *valid = false;
    }
    for (slot, base) in [(1, Type::Int), (2, Type::Text)] {
        let applies = matches!(
            operation,
            "mean" | "median" | "variance" | "standard_deviation"
        ) && slot < parameters.len();
        if applies
            && let Some(ty) = argument_type(slot)
            && *ty != Type::Error
            && !is_optional_stat_argument(ty, &base)
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "statistics scale and rounding options have invalid types",
            ));
            *valid = false;
        }
    }
    if operation == "percentile" {
        for (slot, base) in [(3, Type::Int), (4, Type::Text)] {
            if let Some(ty) = argument_type(slot)
                && *ty != Type::Error
                && !is_optional_stat_argument(ty, &base)
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "statistics scale and rounding options have invalid types",
                ));
                *valid = false;
            }
        }
    }
    if operation == "histogram" {
        if let Some(ty) = argument_type(1)
            && !matches!(ty, Type::List(bins) if matches!(bins.as_ref(), Type::Tuple(parts) if parts.len() == 2 && parts[0] == element && parts[1] == element))
            && *ty != Type::Error
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "std.stats.histogram bins must be a list of matching boundary pairs",
            ));
            *valid = false;
        }
        if let Some(ty) = argument_type(2)
            && !matches!(ty, Type::Bool | Type::Error)
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "std.stats.histogram include_final_upper must be Bool",
            ));
            *valid = false;
        }
    }
    match operation {
        "sum" => element.clone(),
        "mode" => Type::List(Box::new(element.clone())),
        "histogram" => Type::List(Box::new(Type::Int)),
        "derivative" => Type::List(Box::new(element.clone())),
        "mean" | "median" | "percentile" | "min" | "max" | "range" | "variance"
        | "standard_deviation" | "rate" | "integrate" => {
            Type::Optional(Box::new(if matches!(operation, "rate" | "integrate") {
                match element {
                    Type::Tuple(parts) if parts.len() == 2 => {
                        if operation == "integrate" {
                            integrate_quantity_type(&parts[1], scope, diagnostics)
                        } else {
                            parts[1].clone()
                        }
                    }
                    _ => Type::Error,
                }
            } else {
                affine_statistic_type(operation, &element)
            }))
        }
        _ => Type::Error,
    }
}

/// The result of a spread statistic over absolute affine values
/// (ORNA-UNIT-007): `range` and `standard_deviation` are the corresponding
/// delta quantity and `variance` is its square. Every other statistic, and
/// every statistic over linear values, keeps the element type.
pub(crate) fn affine_statistic_type(operation: &str, element: &Type) -> Type {
    let Type::Quantity {
        numeric,
        dimension_basis,
        scale,
        affine: true,
        ..
    } = element
    else {
        return element.clone();
    };
    let delta = Type::Quantity {
        numeric: numeric.clone(),
        dimension_basis: dimension_basis.clone(),
        scale: scale.clone(),
        offset: UnitFraction::zero().public(),
        affine: false,
    };
    match operation {
        "range" | "standard_deviation" => delta,
        "variance" => quantity_product_type(false, &delta, &delta).unwrap_or(Type::Error),
        _ => element.clone(),
    }
}

pub(crate) fn integrate_quantity_type(
    value: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    let Type::Quantity {
        numeric,
        dimension_basis,
        scale,
        offset,
        affine,
    } = value
    else {
        return value.clone();
    };
    if *affine {
        diagnostics.push(diag(
            DIAG_TYPE,
            "std.stats.integrate of absolute affine values needs an explicit linear reference",
        ));
        return Type::Error;
    }
    let time_dimensions = dimension_basis
        .keys()
        .filter(|dimension| dimension.name == "Time")
        .cloned()
        .collect::<BTreeSet<_>>();
    let time_dimension = match time_dimensions.len() {
        1 => time_dimensions.into_iter().next(),
        0 => {
            let time_dimensions = visible_units(scope)
                .flat_map(|unit| unit.dimension_basis.keys())
                .filter(|dimension| dimension.name == "Time")
                .cloned()
                .collect::<BTreeSet<_>>();
            (time_dimensions.len() == 1)
                .then(|| time_dimensions.into_iter().next())
                .flatten()
        }
        _ => None,
    };
    let Some(time_dimension) = time_dimension else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "std.stats.integrate requires one unambiguous declared Time dimension for source-unit quantities",
        ));
        return Type::Error;
    };
    let Some(time_exponent) = dimension_basis
        .get(&time_dimension)
        .copied()
        .unwrap_or_default()
        .checked_add(1)
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "std.stats.integrate unit dimension exponent exceeds the supported range",
        ));
        return Type::Error;
    };
    let mut dimension_basis = dimension_basis.clone();
    if time_exponent == 0 {
        dimension_basis.remove(&time_dimension);
    } else {
        dimension_basis.insert(time_dimension, time_exponent);
    }
    Type::Quantity {
        numeric: numeric.clone(),
        dimension_basis,
        scale: scale.clone(),
        offset: offset.clone(),
        affine: *affine,
    }
}

pub(crate) fn is_statistics_numeric_type(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Decimal | Type::Float)
        || matches!(ty, Type::Applied { base, arguments } if matches!(base.as_str(), "Int" | "Decimal" | "Float") && arguments.len() == 1)
        || matches!(ty, Type::Quantity { numeric, .. } if is_statistics_numeric_type(numeric))
}

pub(crate) fn is_optional_stat_argument(ty: &Type, inner: &Type) -> bool {
    ty == inner || ty == &Type::Null || ty == &Type::Optional(Box::new(inner.clone()))
}

pub(crate) fn is_non_positive_integer_constant(expression: &Expr) -> bool {
    match expression {
        Expr::Literal {
            text,
            kind: LiteralKind::Integer,
            ..
        } => text == "0",
        Expr::Unary { op, rhs, .. } if op == "-" => matches!(
            rhs.as_ref(),
            Expr::Literal {
                kind: LiteralKind::Integer,
                ..
            }
        ),
        Expr::Group { inner, .. } => is_non_positive_integer_constant(inner),
        _ => false,
    }
}

pub(crate) fn is_negative_integer_constant(expression: &Expr) -> bool {
    match expression {
        Expr::Unary { op, rhs, .. } if op == "-" => matches!(
            rhs.as_ref(),
            Expr::Literal {
                kind: LiteralKind::Integer,
                ..
            }
        ),
        Expr::Group { inner, .. } => is_negative_integer_constant(inner),
        _ => false,
    }
}

/// Applies one of the parsed success-pipeline forms used by the reference
/// project. It models argument-one insertion only; it does not invoke stages.
pub(crate) fn infer_success_pipeline(
    lhs: &Expr,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let input = if is_finite_list_numeric_aggregate_pipeline_stage(rhs, scope, local) {
        infer_finite_list_aggregate_rows(lhs, None, scope, local, diagnostics)
    } else {
        infer(lhs, scope, local, diagnostics)
    };
    if let Some(result) = pipeline_generic_stage(&input, rhs, scope, local, diagnostics) {
        return result;
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == DIAG_DURABLE_CONSUMER_SOURCE_ROOTS)
    {
        return Inferred::error_with(input.effects);
    }
    if let Some(result) = pipeline_relation_module_stage(&input, rhs, scope, local, diagnostics) {
        return result;
    }
    if let Some(result) = pipeline_relation_intrinsic_stage(&input, rhs, scope, local, diagnostics)
    {
        return result;
    }
    if let Some(result) = pipeline_collection_stage(&input, rhs, scope, local, diagnostics) {
        return result;
    }
    if let Some(result) = pipeline_untyped_stage(&input, rhs, scope, local, diagnostics) {
        return result;
    }
    infer_relation_stream_pipeline(input, rhs, scope, local, diagnostics)
}

pub(crate) fn pipeline_generic_stage(
    input: &Inferred,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if let Expr::GenericCall {
        callee,
        type_arguments,
        arguments,
        ..
    } = rhs
    {
        return Some(infer_generic_pipeline_stage(
            input.clone(),
            callee,
            type_arguments,
            arguments,
            scope,
            local,
            diagnostics,
        ));
    }
    if let Expr::Call {
        callee, arguments, ..
    } = rhs
        && qualified_path(callee).as_deref() == Some(["sys", "explain"].as_slice())
    {
        let functions = system_api::embedded_system_api()
            .function("sys.explain")
            .expect("descriptor-driven sys.explain path has a descriptor");
        let stage = infer_query_explain_system_call(
            functions,
            arguments,
            None,
            Some(&input.ty),
            scope,
            local,
            diagnostics,
        )
        .expect("sys.explain has a query-plan overload");
        let mut effects = input.effects.clone();
        effects.join(&stage.effects);
        return Some(Inferred {
            ty: stage.ty,
            effects,
        });
    }
    if let Expr::Call {
        callee, arguments, ..
    } = rhs
        && qualified_path(callee).as_deref() == Some(["sys", "cancel"].as_slice())
    {
        let functions = system_api::embedded_system_api()
            .function("sys.cancel")
            .expect("descriptor-driven sys.cancel path has a descriptor");
        let stage = infer_cancel_system_call(
            functions,
            arguments,
            None,
            Some(&input.ty),
            scope,
            local,
            diagnostics,
        );
        let mut effects = input.effects.clone();
        effects.join(&stage.effects);
        return Some(Inferred {
            ty: stage.ty,
            effects,
        });
    }
    None
}

pub(crate) fn pipeline_relation_module_stage(
    input: &Inferred,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if !matches!(&input.ty, Type::Relation(_)) {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = rhs
    else {
        return None;
    };
    let resolved = resolve_intrinsic(callee, scope, local)?;
    if resolved.root || !is_registry_typed(resolved.id) {
        return None;
    }
    Some(check_declared_aggregate_element(
        callee,
        resolved,
        infer_intrinsic(
            resolved.id,
            Some(input.clone()),
            arguments,
            scope,
            local,
            diagnostics,
        ),
        scope,
        diagnostics,
    ))
}

pub(crate) fn pipeline_relation_intrinsic_stage(
    input: &Inferred,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if !matches!(&input.ty, Type::Relation(_)) {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = rhs
    else {
        return None;
    };
    let resolved = resolve_intrinsic(callee, scope, local)?;
    if !resolved.root
        || !is_registry_typed(resolved.id)
        || orna_intrinsics::by_id(resolved.id).module == orna_intrinsics::Module::Stats
    {
        return None;
    }
    if resolved.id == orna_intrinsics::IntrinsicId::CollectionWindow
        && !(1..=2).contains(&arguments.len())
    {
        let mut effects = input.effects.clone();
        for argument in arguments {
            effects.join(&infer(&argument.value, scope, local, diagnostics).effects);
        }
        diagnostics.push(diag(
            DIAG_TYPE,
            "window arguments do not match its static signature",
        ));
        return Some(Inferred::error_with(effects));
    }
    if !relation_pipeline_shape_claimed(resolved.id, arguments) {
        return None;
    }
    let result = infer_intrinsic(
        resolved.id,
        Some(input.clone()),
        arguments,
        scope,
        local,
        diagnostics,
    );
    if resolved.id == orna_intrinsics::IntrinsicId::CoreFlatMap
        && arguments.iter().any(|argument| {
            argument
                .name
                .as_deref()
                .is_some_and(|name| name != "transform")
        })
    {
        diagnostics.push(diag(
            DIAG_TYPE,
            "relation flat_map argument name does not match its static signature",
        ));
    }
    Some(result)
}

pub(crate) fn pipeline_collection_stage(
    input: &Inferred,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if matches!(&input.ty, Type::List(_))
        && let Expr::Call {
            callee, arguments, ..
        } = rhs
        && let Some(resolved) = resolve_intrinsic(callee, scope, local)
        && is_registry_typed(resolved.id)
        && orna_intrinsics::by_id(resolved.id).module != orna_intrinsics::Module::Stats
        && (!resolved.root || list_pipeline_shape_claimed(resolved.id, arguments))
    {
        return Some(check_declared_aggregate_element(
            callee,
            resolved,
            infer_intrinsic(
                resolved.id,
                Some(input.clone()),
                arguments,
                scope,
                local,
                diagnostics,
            ),
            scope,
            diagnostics,
        ));
    }
    if let Type::List(element) = &input.ty
        && let Expr::Call {
            callee, arguments, ..
        } = rhs
        && let Expr::Name { text, .. } = callee.as_ref()
    {
        match (text.as_str(), arguments.as_slice()) {
            ("filter", [argument])
                if argument.name.is_none() || argument.name.as_deref() == Some("predicate") =>
            {
                let callback = infer_callback(
                    &argument.value,
                    element.as_ref().clone(),
                    Type::Bool,
                    scope,
                    local,
                    diagnostics,
                );
                let mut effects = input.effects.clone();
                effects.join(&callback.effects);
                return Some(Inferred {
                    ty: if callback.ty == Type::Bool {
                        input.ty.clone()
                    } else {
                        Type::Error
                    },
                    effects,
                });
            }
            ("distinct", []) => {
                if is_default_float_equality_type(element) {
                    diagnostics.push(diag(
                    DIAG_TYPE,
                    "distinct requires a lawful equality key; default Float equality is unavailable",
                ));
                    return Some(Inferred::error_with(input.effects.clone()));
                }
                return Some(Inferred {
                    ty: input.ty.clone(),
                    effects: input.effects.clone(),
                });
            }
            ("union", [argument])
                if argument.name.is_none() || argument.name.as_deref() == Some("right") =>
            {
                let right = infer(&argument.value, scope, local, diagnostics);
                let mut effects = input.effects.clone();
                effects.join(&right.effects);
                let Type::List(right_element) = right.ty else {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "union requires a list with a compatible element type",
                    ));
                    return Some(Inferred::error_with(effects));
                };
                if !types_match(element, &right_element) {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "union requires lists with compatible element types",
                    ));
                    return Some(Inferred::error_with(effects));
                }
                return Some(Inferred {
                    ty: input.ty.clone(),
                    effects,
                });
            }
            ("sort_by", [argument]) if argument.name.is_none() => {
                let callback = infer_callback(
                    &argument.value,
                    element.as_ref().clone(),
                    Type::Error,
                    scope,
                    local,
                    diagnostics,
                );
                let mut effects = input.effects.clone();
                effects.join(&callback.effects);
                return Some(Inferred {
                    ty: input.ty.clone(),
                    effects,
                });
            }
            ("last", []) => {
                return Some(Inferred {
                    ty: Type::Optional(element.clone()),
                    effects: input.effects.clone(),
                });
            }
            _ => {}
        }
    }
    if let Expr::Name { text, .. } = rhs
        && text == "distinct"
        && let Type::List(element) = &input.ty
    {
        if is_default_float_equality_type(element) {
            diagnostics.push(diag(
                DIAG_TYPE,
                "distinct requires a lawful equality key; default Float equality is unavailable",
            ));
            return Some(Inferred::error_with(input.effects.clone()));
        }
        return Some(Inferred {
            ty: input.ty.clone(),
            effects: input.effects.clone(),
        });
    }
    if let Expr::Call {
        callee, arguments, ..
    } = rhs
        && matches!(callee.as_ref(), Expr::Name { text, .. } if text == "take" || text == "drop")
        && let Type::List(element) = &input.ty
    {
        let mut effects = input.effects.clone();
        let operation = match callee.as_ref() {
            Expr::Name { text, .. } => text.as_str(),
            _ => unreachable!("pipeline collection operation must be named"),
        };
        let ty = match arguments.as_slice() {
            [argument] if argument.name.is_none() || argument.name.as_deref() == Some("count") => {
                let count = infer(&argument.value, scope, local, diagnostics);
                effects.join(&count.effects);
                if count.ty == Type::Int && !is_negative_integer_constant(&argument.value)
                    || matches!(&count.ty, Type::Range(element) if element.as_ref() == &Type::Int)
                {
                    Type::List(element.clone())
                } else if count.ty == Type::Int {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        format!("{operation} count must be nonnegative"),
                    ));
                    Type::Error
                } else {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        format!("{operation} requires an integer count"),
                    ));
                    Type::Error
                }
            }
            _ => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("{operation} requires one integer count"),
                ));
                Type::Error
            }
        };
        return Some(Inferred { ty, effects });
    }
    if let Type::List(element) = &input.ty
        && let Expr::Name { text, .. } = rhs
        && (text == "mean" || root_intrinsic_id(text, scope, local).is_some())
        && let Some(ty) = infer_affine_collection_aggregate(text, element, diagnostics)
    {
        return Some(Inferred {
            ty,
            effects: input.effects.clone(),
        });
    }
    if let Type::List(_) = &input.ty
        && let Some(resolved) = resolve_intrinsic(rhs, scope, local)
        && matches!(
            resolved.id,
            orna_intrinsics::IntrinsicId::CoreSum
                | orna_intrinsics::IntrinsicId::CoreMin
                | orna_intrinsics::IntrinsicId::CoreMax
        )
    {
        return Some(infer_intrinsic(
            resolved.id,
            Some(input.clone()),
            &[],
            scope,
            local,
            diagnostics,
        ));
    }
    if let Expr::Name { text, .. } = rhs
        && let Type::Relation(element) = &input.ty
        && root_intrinsic_id(text, scope, local).is_some()
        && let Some(ty) = infer_relation_sum(text, element, diagnostics)
    {
        return Some(Inferred {
            ty,
            effects: input.effects.clone(),
        });
    }
    if matches!(&input.ty, Type::Relation(_) | Type::Stream(_))
        && let Some((callee, arguments)) = match rhs {
            Expr::Call {
                callee, arguments, ..
            } => Some((callee.as_ref(), arguments.as_slice())),
            Expr::Name { .. } | Expr::Field { .. } => Some((rhs, &[][..])),
            _ => None,
        }
        && let Expr::Name { text, .. } = callee
        && matches!(
            text.as_str(),
            "filter"
                | "every"
                | "exists"
                | "first"
                | "one"
                | "map"
                | "flat_map"
                | "sort_by"
                | "take"
                | "drop"
                | "count"
                | "window"
                | "sum"
                | "min"
                | "max"
        )
        && root_intrinsic_id(text, scope, local).is_none()
    {
        return Some(infer_named_pipeline_stage(
            input.clone(),
            callee,
            arguments,
            scope,
            local,
            diagnostics,
        ));
    }
    if let Expr::Name { text, .. } = rhs
        && text == "count"
        && matches!(input.ty, Type::List(_))
    {
        return Some(Inferred {
            ty: Type::Int,
            effects: input.effects.clone(),
        });
    }
    if let Expr::Name { text, .. } = rhs
        && text == "count"
        && matches!(input.ty, Type::Relation(_))
        && root_intrinsic_id("count", scope, local).is_some()
    {
        return Some(Inferred {
            ty: Type::Int,
            effects: input.effects.clone(),
        });
    }
    None
}

pub(crate) fn pipeline_untyped_stage(
    input: &Inferred,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if !matches!(input.ty, Type::Relation(_) | Type::Stream(_)) {
        if let Some(lambda) = pipeline_lambda(rhs) {
            return Some(infer_lambda_pipeline_stage(
                input.clone(),
                lambda,
                scope,
                local,
                diagnostics,
            ));
        }
        return Some(match rhs {
            Expr::Call {
                callee, arguments, ..
            } => infer_named_pipeline_stage(
                input.clone(),
                callee,
                arguments,
                scope,
                local,
                diagnostics,
            ),
            Expr::Name { .. } | Expr::Field { .. } => {
                infer_named_pipeline_stage(input.clone(), rhs, &[], scope, local, diagnostics)
            }
            _ => {
                diagnostics.push(diag(
                    DIAG_UNSUPPORTED,
                    "pipeline stage is outside this semantic slice",
                ));
                Inferred::error_with(input.effects.clone())
            }
        });
    }
    None
}

pub(crate) fn infer_relation_stream_pipeline(
    input: Inferred,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Call {
        callee, arguments, ..
    } = rhs
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline stage is outside this semantic slice",
        ));
        return input;
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        // ORNA-PIPE-004: any callable may be a stage. A qualified callee such
        // as `alias.op` is checked as a named callable with the piped value as
        // its first argument, the same path the list pipeline uses.
        return infer_named_pipeline_stage(input, callee, arguments, scope, local, diagnostics);
    };
    let (element, is_stream) = match &input.ty {
        Type::Relation(element) => (element.as_ref().clone(), false),
        Type::Stream(element) => (element.as_ref().clone(), true),
        _ => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "pipeline input is not a supported relation or stream",
            ));
            return Inferred::error_with(input.effects);
        }
    };
    if !is_stream
        && text == "map"
        && root_intrinsic_id(text, scope, local).is_some()
        && let [argument] = arguments.as_slice()
        && (argument.name.is_none() || argument.name.as_deref() == Some("transform"))
    {
        if let Some(projection) = infer_table_projection(&argument.value, scope, local) {
            let Type::Function {
                parameters, result, ..
            } = &projection.ty
            else {
                unreachable!("table projection is always callable");
            };
            let shared_contact_selector = matches!(
                &argument.value,
                Expr::Field { name, .. } if name == "full_name"
            );
            if !(types_match(&parameters[0], &element)
                || shared_contact_selector
                    && table_row_types_match(&parameters[0], &element, scope))
            {
                require_same(&parameters[0], &element, diagnostics);
            }
            let mut effects = input.effects;
            effects.join(&projection.effects);
            return Inferred {
                ty: Type::Relation(result.clone()),
                effects,
            };
        }
        let callback = infer_relation_callback(
            &argument.value,
            element,
            Type::Error,
            scope,
            local,
            diagnostics,
        );
        let mut effects = input.effects;
        effects.join(&callback.effects);
        return Inferred {
            ty: Type::Relation(Box::new(callback.ty)),
            effects,
        };
    }
    if !is_stream
        && matches!(text.as_str(), "take" | "drop")
        && root_intrinsic_id(text, scope, local).is_some()
        && let [argument] = arguments.as_slice()
        && (argument.name.is_none() || argument.name.as_deref() == Some("count"))
    {
        let count = infer(&argument.value, scope, local, diagnostics);
        let mut effects = input.effects;
        effects.join(&count.effects);
        if count.ty != Type::Int && count.ty != Type::Error {
            diagnostics.push(diag(
                DIAG_TYPE,
                format!("relation {text} count must be an Int"),
            ));
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                format!("relation {text} count is outside the supported bounded form"),
            ));
            return Inferred::error_with(effects);
        }
        return Inferred {
            ty: Type::Relation(Box::new(element)),
            effects,
        };
    }
    if !is_stream && text == "flat_map" && root_intrinsic_id("flat_map", scope, local).is_some() {
        let [argument] = arguments.as_slice() else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "relation flat_map requires one transform callback",
            ));
            return Inferred::error_with(input.effects);
        };
        let malformed = argument.name.is_some() && argument.name.as_deref() != Some("transform");
        if malformed {
            diagnostics.push(diag(
                DIAG_TYPE,
                "relation flat_map argument name does not match its static signature",
            ));
        }
        let callback = infer_relation_callback(
            &argument.value,
            element,
            Type::Error,
            scope,
            local,
            diagnostics,
        );
        let mut effects = input.effects;
        effects.join(&callback.effects);
        let output = match callback.ty {
            Type::List(element) | Type::Relation(element) => *element,
            Type::Error => Type::Error,
            _ => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "relation flat_map transform must return a finite collection",
                ));
                return Inferred::error_with(effects);
            }
        };
        return Inferred {
            ty: if malformed {
                Type::Error
            } else {
                Type::Relation(Box::new(output))
            },
            effects,
        };
    }
    let (ty, callback_result) = match (text.as_str(), is_stream, arguments.as_slice()) {
        ("filter", false, [argument])
            if (argument.name.is_none() || argument.name.as_deref() == Some("predicate"))
                && root_intrinsic_id("filter", scope, local).is_some() =>
        {
            (Type::Relation(Box::new(element.clone())), Some(Type::Bool))
        }
        ("distinct", false, []) if root_intrinsic_id("distinct", scope, local).is_some() => {
            if is_default_float_equality_type(&element) {
                diagnostics.push(diag(
                DIAG_TYPE,
                "distinct requires a lawful equality key; default Float equality is unavailable",
            ));
                (Type::Error, None)
            } else {
                (Type::Relation(Box::new(element.clone())), None)
            }
        }
        ("one", false, []) if root_intrinsic_id("one", scope, local).is_some() => {
            (element.clone(), None)
        }
        ("one", false, [_]) if root_intrinsic_id("one", scope, local).is_some() => {
            (element.clone(), Some(Type::Bool))
        }
        ("last", false, []) => (Type::Optional(Box::new(element.clone())), None),
        ("count", false, []) if root_intrinsic_id("count", scope, local).is_some() => {
            (Type::Int, None)
        }
        ("pairs", false, []) => (
            Type::Relation(Box::new(Type::Tuple(vec![
                element.clone(),
                element.clone(),
            ]))),
            None,
        ),
        ("min", false, []) | ("max", false, [])
            if root_intrinsic_id(text, scope, local).is_some() =>
        {
            (Type::Optional(Box::new(element.clone())), None)
        }
        ("for_each", true, [_]) => (Type::Null, Some(Type::Error)),
        ("bucket_by", false, _) => (Type::Relation(Box::new(element.clone())), None),
        _ => {
            let ignored = infer(rhs, scope, local, diagnostics);
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "pipeline operation is outside this semantic slice",
            ));
            return Inferred::error_with({
                let mut effects = input.effects;
                effects.join(&ignored.effects);
                effects
            });
        }
    };
    let mut effects = input.effects;
    if text == "bucket_by" && !is_stream {
        let mut values = Vec::new();
        for argument in arguments {
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
            values.push(value.ty);
        }
        let row = match &element {
            Type::Named(name) => scope.table_rows.get(name).unwrap_or(&element),
            _ => &element,
        };
        let instant = row == &Type::Instant
            || matches!(row, Type::Record(fields) if fields.get("time") == Some(&Type::Instant));
        let positional_period = arguments
            .first()
            .is_some_and(|argument| argument.name.is_none());
        let calendar = values.first().is_some_and(is_calendar_period);
        let elapsed = values
            .first()
            .is_some_and(|ty| ty == &Type::Named("std.DURATION".into()));
        let zone = arguments.len() == 2
            && arguments[1].name.as_deref() == Some("zone")
            && values[1] == Type::Named("std.TimeZone".into());
        let message = if !instant {
            Some((
                DIAG_TYPE,
                "bucket_by requires Instant values or rows with an Instant time field",
            ))
        } else if !positional_period || !(calendar || elapsed) {
            Some((
                DIAG_TYPE,
                "bucket_by requires an elapsed Duration or calendar-day period",
            ))
        } else if calendar && arguments.len() == 1 {
            Some((
                DIAG_CALENDAR_BUCKET_ZONE,
                "calendar bucketing of Instant requires a time zone",
            ))
        } else if !(zone || elapsed && arguments.len() == 1) {
            Some((DIAG_TYPE, "bucket_by requires a named TimeZone argument"))
        } else {
            None
        };
        if let Some((code, message)) = message {
            diagnostics.push(diag(code, message));
            return Inferred::error_with(effects);
        }
        return Inferred { ty, effects };
    }
    if let Some(result) = callback_result {
        let callback = if is_stream {
            infer_callback(
                &arguments[0].value,
                element,
                result,
                scope,
                local,
                diagnostics,
            )
        } else {
            infer_relation_callback(
                &arguments[0].value,
                element,
                result,
                scope,
                local,
                diagnostics,
            )
        };
        effects.join(&callback.effects);
    }
    if text == "one" {
        effects.may_fail = true;
    }
    Inferred { ty, effects }
}

pub(crate) fn is_default_float_equality_type(ty: &Type) -> bool {
    match ty {
        Type::Float => true,
        Type::Applied { base, arguments } => {
            base == "Float" || arguments.iter().any(is_default_float_equality_type)
        }
        Type::Quantity { numeric, .. } => is_default_float_equality_type(numeric),
        Type::List(element) | Type::Optional(element) | Type::Range(element) => {
            is_default_float_equality_type(element)
        }
        Type::Tuple(elements) => elements.iter().any(is_default_float_equality_type),
        Type::Record(fields) => fields.values().any(is_default_float_equality_type),
        Type::MoneyPerUnit { currency, unit } => {
            is_default_float_equality_type(currency) || is_default_float_equality_type(unit)
        }
        Type::Relation(_) | Type::Stream(_) | Type::Function { .. } => false,
        Type::Int
        | Type::Decimal
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Named(_)
        | Type::Bottom
        | Type::Error => false,
    }
}

pub(crate) fn infer_recovery_pipeline(
    lhs: &Expr,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let input = infer(lhs, scope, local, diagnostics);
    let Some(expression) = pipeline_lambda(rhs) else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "recovery pipeline requires a one-parameter lambda",
        ));
        return Inferred::error_with(input.effects);
    };
    let Expr::Lambda {
        parameters, body, ..
    } = expression
    else {
        unreachable!("pipeline_lambda only returns lambda expressions");
    };
    let [parameter] = parameters.as_slice() else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "recovery pipeline callback must take one parameter",
        ));
        return Inferred::error_with(input.effects);
    };
    let Pattern::Name(name, _) = &parameter.pattern else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "recovery pipeline callback pattern is outside this semantic slice",
        ));
        return Inferred::error_with(input.effects);
    };
    let mut callback_locals = local.clone();
    insert_local_binding(name, error_value_type(), &mut callback_locals, diagnostics);
    let recovered = infer_function_body(body, None, scope, &callback_locals, diagnostics);
    let mut effects = input.effects;
    effects
        .effects
        .extend(recovered.effects.effects.iter().copied());
    effects.opaque |= recovered.effects.opaque;
    effects.may_fail = recovered.effects.may_fail;
    let ty = if recovered.ty == Type::Bottom {
        input.ty
    } else if input.ty == Type::Bottom || types_match(&input.ty, &recovered.ty) {
        recovered.ty
    } else {
        require_same(&input.ty, &recovered.ty, diagnostics);
        Type::Error
    };
    Inferred { ty, effects }
}

pub(crate) fn infer_fail_call(
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary {
        may_fail: true,
        ..EffectSummary::default()
    };
    let values = arguments
        .iter()
        .map(|argument| {
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();
    let valid_shape = arguments.len() == 1
        && arguments[0]
            .name
            .as_deref()
            .is_none_or(|name| name == "error_value");
    if !valid_shape {
        diagnostics.push(diag(DIAG_TYPE, "fail requires exactly one Error argument"));
    }
    let valid_type = values
        .first()
        .is_some_and(|value| types_match(&error_value_type(), value));
    if valid_shape && !valid_type {
        diagnostics.push(diag(DIAG_TYPE, "fail argument must have type Error"));
    }
    Inferred {
        ty: if valid_shape && valid_type {
            Type::Bottom
        } else {
            Type::Error
        },
        effects,
    }
}

pub(crate) fn infer_error_call(
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    let values = arguments
        .iter()
        .map(|argument| {
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();

    if arguments.iter().any(|argument| argument.name.is_none()) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "error requires named code, message, and optional cause arguments",
        ));
        return Inferred::error_with(effects);
    }

    let before = diagnostics.len();
    check_call_arguments(
        &[Type::Text, Type::Text, error_value_type()],
        Some(&["code".into(), "message".into(), "cause".into()]),
        &BTreeSet::from([2]),
        arguments,
        &values,
        None,
        diagnostics,
    );

    Inferred {
        ty: if diagnostics.len() == before {
            error_value_type()
        } else {
            Type::Error
        },
        effects,
    }
}

pub(crate) fn error_value_type() -> Type {
    Type::Named("Error".into())
}

/// Static field types of the portable `Error` value (ORNA-FAILURE-005).
/// `safe_details` is a text-keyed map of arbitrary values with no static
/// field type, so it is intentionally not listed.
pub(crate) fn error_value_field_type(nominal: &str, name: &str) -> Option<Type> {
    if nominal != "Error" {
        return None;
    }
    match name {
        "code" | "message" => Some(Type::Text),
        "causes" => Some(Type::List(Box::new(error_value_type()))),
        _ => None,
    }
}

pub(crate) fn pipeline_lambda(expression: &Expr) -> Option<&Expr> {
    match expression {
        Expr::Lambda { .. } => Some(expression),
        Expr::Group { inner, .. } => pipeline_lambda(inner),
        _ => None,
    }
}

pub(crate) fn infer_lambda_pipeline_stage(
    input: Inferred,
    expression: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Lambda {
        parameters, body, ..
    } = expression
    else {
        unreachable!("pipeline_lambda only returns lambda expressions");
    };
    let [parameter] = parameters.as_slice() else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline lambda must take exactly one parameter",
        ));
        return Inferred::error_with(input.effects);
    };
    let Pattern::Name(name, _) = &parameter.pattern else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline lambda parameter pattern is outside this semantic slice",
        ));
        return Inferred::error_with(input.effects);
    };
    let parameter_type = parameter
        .annotation
        .as_ref()
        .map(type_of)
        .unwrap_or_else(|| input.ty.clone());
    require_same(&parameter_type, &input.ty, diagnostics);
    let mut lambda_locals = local.clone();
    insert_local_binding(name, parameter_type, &mut lambda_locals, diagnostics);
    let body = infer_function_body(body, None, scope, &lambda_locals, diagnostics);
    let mut effects = input.effects;
    effects.join(&body.effects);
    Inferred {
        ty: body.ty,
        effects,
    }
}

/// Applies a named callable pipeline stage by checking the implicit input as
/// its first positional argument. Relation and stream stages are intentionally
/// handled by `infer_success_pipeline`'s closed intrinsic path above.
pub(crate) fn infer_named_pipeline_stage(
    input: Inferred,
    callee: &Expr,
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Expr::Name { text, .. } = callee
        && root_intrinsic_id(text, scope, local).is_some()
        && !matches!(
            input.ty,
            Type::Relation(_)
                | Type::Stream(_)
                | Type::List(_)
                | Type::Range(_)
                | Type::Applied { .. }
        )
    {
        // ORNA-PIPE-004 admits a stage only when its first parameter accepts the
        // piped value. The closed collection intrinsics take a collection kind
        // (09-standard-library), so a scalar or structural input is rejected by
        // this clause rather than by resolving the intrinsic name. Applied
        // constructors keep their own resolution. A failed input is not piped
        // into the stage (ORNA-PIPE-007), so it adds no diagnostic of its own.
        let mut effects = input.effects;
        for argument in arguments {
            if !matches!(argument.value, Expr::Lambda { .. }) {
                let value = infer(&argument.value, scope, local, diagnostics);
                effects.join(&value.effects);
            }
        }
        if !matches!(input.ty, Type::Error) {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                format!("pipeline stage {text} requires a collection input"),
            ));
        }
        return Inferred::error_with(effects);
    }
    if qualified_path(callee).is_none() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline stage must be a named callable",
        ));
        return Inferred::error_with(input.effects);
    }
    let callee = infer(callee, scope, local, diagnostics);
    let mut effects = input.effects;
    effects.join(&callee.effects);
    let callee_is_error = matches!(callee.ty, Type::Error);
    let Type::Function {
        parameters,
        parameter_names,
        result,
        default_parameters,
    } = callee.ty
    else {
        for argument in arguments {
            let value = if matches!(argument.value, Expr::Lambda { .. }) {
                Inferred::error()
            } else {
                infer(&argument.value, scope, local, diagnostics)
            };
            effects.join(&value.effects);
        }
        if !callee_is_error {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "pipeline stage is not a supported named callable",
            ));
        }
        return Inferred::error_with(effects);
    };
    let mut values = Vec::with_capacity(arguments.len() + 1);
    values.push(input.ty);
    for (index, argument) in arguments.iter().enumerate() {
        let expected = expected_pipeline_call_parameter(
            &parameters,
            parameter_names.as_deref(),
            arguments,
            index,
        );
        let value = match expected {
            Some(expected @ Type::Function { .. }) => {
                infer_contextual(&argument.value, expected, scope, local, diagnostics)
            }
            Some(_) if matches!(argument.value, Expr::Lambda { .. }) => Inferred::error(),
            Some(expected) => {
                infer_contextual(&argument.value, expected, scope, local, diagnostics)
            }
            None if matches!(argument.value, Expr::Lambda { .. }) => Inferred::error(),
            None => infer(&argument.value, scope, local, diagnostics),
        };
        effects.join(&value.effects);
        values.push(value.ty);
    }
    check_call_arguments(
        &parameters,
        parameter_names.as_deref(),
        &default_parameters,
        arguments,
        &values[1..],
        values.first(),
        diagnostics,
    );
    Inferred {
        ty: *result,
        effects,
    }
}

pub(crate) fn expected_pipeline_call_parameter<'a>(
    parameters: &'a [Type],
    parameter_names: Option<&'a [String]>,
    arguments: &[orna_syntax_ast::Argument],
    index: usize,
) -> Option<&'a Type> {
    let argument = arguments.get(index)?;
    if let Some(name) = argument.name.as_deref() {
        return parameter_names?
            .iter()
            .position(|parameter| parameter == name)
            .and_then(|position| parameters.get(position));
    }
    let positional = arguments[..index]
        .iter()
        .filter(|argument| argument.name.is_none())
        .count();
    parameters.get(positional + 1)
}

pub(crate) fn infer_generic_pipeline_stage(
    input: Inferred,
    callee: &Expr,
    type_arguments: &[TypeExpr],
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = input.effects;
    let Some(path) = qualified_path(callee) else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "generic pipeline stage must be a supported named callable",
        ));
        return Inferred::error_with(effects);
    };
    if path == ["sys", "explain"]
        || path == ["sys", "meta"]
        || path == ["sys", "await"]
        || path == ["sys", "cancel"]
        || path == ["sys", "invoke"]
        || path == ["sys", "start"]
    {
        let stage = infer_descriptor_system_call_with_input(
            &path,
            arguments,
            type_arguments,
            &input.ty,
            scope,
            local,
            diagnostics,
        )
        .expect("known system generic pipeline path has a descriptor");
        effects.join(&stage.effects);
        return Inferred {
            ty: stage.ty,
            effects,
        };
    }
    if let Some(currency) = money_generic_constructor_currency(callee, type_arguments, scope) {
        if arguments.is_empty() {
            let stage = infer_money_constructor_type(currency, &input.ty, scope, diagnostics);
            effects.join(&stage.effects);
            return Inferred {
                ty: stage.ty,
                effects,
            };
        }
        for argument in arguments {
            effects.join(&infer(&argument.value, scope, local, diagnostics).effects);
        }
        diagnostics.push(diag(
            DIAG_TYPE,
            "Money constructor pipeline stage requires the piped value as its only argument",
        ));
        return Inferred::error_with(effects);
    }
    effects.join(&infer(callee, scope, local, diagnostics).effects);
    for argument in arguments {
        effects.join(&infer(&argument.value, scope, local, diagnostics).effects);
    }
    diagnostics.push(diag(
        DIAG_UNSUPPORTED,
        "generic pipeline stage is outside this semantic slice",
    ));
    Inferred::error_with(effects)
}

pub(crate) fn is_ordered_range_bound(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Int | Type::Decimal | Type::Float | Type::Date | Type::Instant
    )
}

/// Types whose direct ordered comparisons the bounded evaluator accepts.
///
/// This deliberately mirrors the evaluator's existing surface rather than
/// defining a broader language-level ordering policy. In particular, the
/// semantic checker leaves its existing Bool and Str behaviour unchanged.
pub(crate) fn is_evaluator_ordered_comparison_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Int
            | Type::Decimal
            | Type::Float
            | Type::Date
            | Type::Instant
            | Type::Text
            | Type::Bool
    ) || matches!(
        ty,
        Type::Range(element) if is_ordered_range_bound(element)
    ) || matches!(
        ty,
        Type::Applied { base, arguments }
            if base == "Money" && arguments.len() == 1
    ) || matches!(
        ty,
        Type::Quantity { numeric, .. }
            if matches!(**numeric, Type::Int | Type::Decimal | Type::Float)
    )
}

/// Applies the affine-absolute aggregation matrix (ORNA-UNIT-007) for the
/// operations outside the integer-only finite-list aggregate slice: selection
/// and `mean` return an absolute value, and `sum` is invalid. Unrecognised
/// collection pipelines retain their prior conservative diagnostics.
pub(crate) fn infer_affine_collection_aggregate(
    operation: &str,
    element: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if !is_absolute_affine(element) {
        return None;
    }
    match operation {
        "mean" => Some(Type::Optional(Box::new(element.clone()))),
        "min" | "max" => Some(Type::Optional(Box::new(element.clone()))),
        "sum" => {
            diagnostics.push(diag(
                DIAG_ABSOLUTE_QUANTITY_SUM,
                "cannot sum absolute affine quantities",
            ));
            Some(Type::Error)
        }
        _ => None,
    }
}

/// Checks the relation `sum` aggregate without coercing exact numeric inputs
/// to binary Float. The matching list rules remain intentionally unchanged.
pub(crate) fn infer_relation_sum(
    operation: &str,
    element: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if operation != "sum" {
        return None;
    }
    if is_absolute_affine(element) {
        diagnostics.push(diag(
            DIAG_ABSOLUTE_QUANTITY_SUM,
            "cannot sum absolute affine quantities",
        ));
        return Some(Type::Error);
    }
    if is_sum_numeric(element) {
        return Some(element.clone());
    }
    diagnostics.push(diag(DIAG_TYPE, "sum requires a numeric element type"));
    Some(Type::Error)
}

pub(crate) fn is_sum_numeric(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Decimal | Type::Float)
        || matches!(
            ty,
            Type::Applied { base, .. }
                if matches!(base.as_str(), "Int" | "Decimal" | "Float" | "Money")
        )
}

pub(crate) fn infer_callback(
    expression: &Expr,
    parameter: Type,
    result: Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Lambda {
        parameters, body, ..
    } = expression
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "core operation callback must be a one-parameter lambda",
        ));
        return Inferred::error();
    };
    let [lambda_parameter] = parameters.as_slice() else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "core operation callback must take one parameter",
        ));
        return Inferred::error();
    };
    let Pattern::Name(name, _) = &lambda_parameter.pattern else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "core operation callback pattern is outside this semantic slice",
        ));
        return Inferred::error();
    };
    let mut callback_locals = local.clone();
    insert_local_binding(name, parameter, &mut callback_locals, diagnostics);
    let inferred = infer_function_body(body, None, scope, &callback_locals, diagnostics);
    require_same(&result, &inferred.ty, diagnostics);
    inferred
}

pub(crate) fn infer_relation_callback(
    expression: &Expr,
    parameter: Type,
    result: Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let callback = if matches!(expression, Expr::Lambda { .. }) {
        infer_callback(
            expression,
            parameter.clone(),
            result.clone(),
            scope,
            local,
            diagnostics,
        )
    } else {
        infer(expression, scope, local, diagnostics)
    };
    if callback.effects.opaque
        || callback
            .effects
            .effects
            .iter()
            .any(|effect| *effect != EffectKind::TableRead)
    {
        diagnostics.push(diag(DIAG_TYPE, "relation query callback must be read-only"));
    }
    if matches!(expression, Expr::Lambda { .. }) {
        return callback;
    }
    let Type::Function {
        parameters,
        result: callback_result,
        ..
    } = callback.ty
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "relation callback must be a one-parameter function",
        ));
        return Inferred::error_with(callback.effects);
    };
    if parameters.len() != 1 {
        diagnostics.push(diag(DIAG_TYPE, "relation callback must take one parameter"));
    } else {
        require_same(&parameter, &parameters[0], diagnostics);
    }
    if result != Type::Error {
        require_same(&result, &callback_result, diagnostics);
    }
    Inferred {
        ty: *callback_result,
        effects: callback.effects,
    }
}

pub(crate) fn infer_finite_list_callback(
    expression: &Expr,
    parameter: Type,
    result: Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if matches!(expression, Expr::Lambda { .. }) {
        return infer_callback(expression, parameter, result, scope, local, diagnostics);
    }
    let callback = infer(expression, scope, local, diagnostics);
    let Type::Function {
        parameters,
        result: callback_result,
        ..
    } = callback.ty
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "finite-list collection callback must be a one-parameter function",
        ));
        return Inferred::error_with(callback.effects);
    };
    if parameters.len() != 1 {
        diagnostics.push(diag(
            DIAG_TYPE,
            "finite-list collection callback must take one parameter",
        ));
    } else {
        require_same(&parameter, &parameters[0], diagnostics);
    }
    require_same(&result, &callback_result, diagnostics);
    Inferred {
        ty: *callback_result,
        effects: callback.effects,
    }
}

/// Resolves the genuine `Relation<T>` instance members from ORNA-MEMBER-005.
/// Table-owned operations stay on their existing path so their schema and
/// operation checks continue to use the table symbol directly.
pub(crate) fn infer_relation_member_call(
    callee: &Expr,
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let Expr::Field { base, name, .. } = callee else {
        return None;
    };
    if !matches!(name.as_str(), "count" | "first") || table_symbol(base, scope, local).is_some() {
        return None;
    }

    let diagnostic_count = diagnostics.len();
    let receiver = infer(base, scope, local, diagnostics);
    let Type::Relation(element) = receiver.ty else {
        diagnostics.truncate(diagnostic_count);
        return None;
    };

    let values = arguments
        .iter()
        .map(|argument| infer(&argument.value, scope, local, diagnostics).ty)
        .collect::<Vec<_>>();
    check_call_arguments(
        &[],
        Some(&[]),
        &BTreeSet::new(),
        arguments,
        &values,
        None,
        diagnostics,
    );

    let mut effects = receiver.effects;
    effects.effects.insert(EffectKind::TableRead);
    effects.may_fail = true;
    Some(Inferred {
        ty: match name.as_str() {
            "count" => Type::Int,
            "first" => Type::Optional(element),
            _ => unreachable!("relation member name was checked above"),
        },
        effects,
    })
}

/// Resolves the small, intrinsic associated-operation surface of a table.
///
/// This deliberately runs only for call expressions: a table is still a
/// relation value in every other expression, and ordinary record field access
/// keeps its existing diagnostics. Provided insertion fields are checked against
/// the row schema; mutation execution remains a separate runtime concern.
pub(crate) fn infer_table_operation(
    callee: &Expr,
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let Expr::Field { base, name, .. } = callee else {
        return None;
    };
    let table = table_symbol(base, scope, local)?;
    let admission = table
        .table_schema
        .as_ref()
        .and_then(|schema| schema.admission.as_ref());
    let authoritative_contact = matches!(
        (&table.ty, admission),
        (
            Type::Named(name),
            Some(TableAdmission {
                computed,
                keys,
                ..
            })
        ) if matches!(name.as_str(), "Contact" | "contacts.Contact")
            && computed.contains("full_name")
            && keys == &[("id".into(), Type::Text)]
    );
    let automatic_key = admission.map_or_else(
        || matches!(&table.ty, Type::Named(table_name) if scope.auto_key_tables.contains(table_name)),
        |schema| schema.automatic_key,
    );
    if name == "rekey" && automatic_key {
        diagnostics.push(diag(
            DIAG_AUTOMATIC_KEY_REKEY,
            "an automatic-key table cannot be explicitly re-keyed",
        ));
    }
    let (parameters, result, effect) = match name.as_str() {
        "insert" | "upsert" => (1, table.ty.clone(), Some(EffectKind::TableWrite)),
        "update" | "rekey" => (2, table.ty.clone(), Some(EffectKind::TableWrite)),
        "delete" => (1, Type::Unit, Some(EffectKind::TableWrite)),
        "count" => (0, Type::Int, Some(EffectKind::TableRead)),
        "first" => (
            0,
            Type::Optional(Box::new(table.ty.clone())),
            Some(EffectKind::TableRead),
        ),
        "one" => (0, table.ty.clone(), Some(EffectKind::TableRead)),
        "as_of" => (
            1,
            Type::Relation(Box::new(table.ty.clone())),
            Some(EffectKind::TableRead),
        ),
        _ => return None,
    };
    if arguments.len() != parameters {
        diagnostics.push(diag(
            DIAG_TYPE,
            "table operation argument count does not match its static signature",
        ));
    }
    let mut effects = EffectSummary {
        effects: BTreeSet::from([effect.expect("all table operations have an effect")]),
        opaque: false,
        may_fail: true,
    };
    let row = table.table_fields().or_else(|| match &table.ty {
        Type::Record(fields) => Some(fields),
        Type::Named(table_name) => match scope.table_rows.get(table_name) {
            Some(Type::Record(fields)) => Some(fields),
            _ => None,
        },
        _ => None,
    });
    for (index, argument) in arguments.iter().enumerate() {
        let insertion = name == "insert";
        let upsert = name == "upsert";
        let insertion_like = insertion || upsert;
        let update = name == "update" && index == 1;
        let key_argument =
            matches!(name.as_str(), "update" | "delete") && index == 0 || name == "rekey";
        let inferred = if name == "as_of" {
            let inferred = infer_contextual(
                &argument.value,
                &Type::Named("sys.SnapshotRef".into()),
                scope,
                local,
                diagnostics,
            );
            require_same(
                &Type::Named("sys.SnapshotRef".into()),
                &inferred.ty,
                diagnostics,
            );
            inferred
        } else if insertion_like || update {
            row.map(|fields| {
                let inferred = infer_table_row_input(
                    &argument.value,
                    fields,
                    scope,
                    local,
                    diagnostics,
                    false,
                );
                if let Some(schema) = admission
                    && let Type::Record(supplied) = &inferred.ty
                {
                    if insertion {
                        for field in &schema.required {
                            if !supplied.contains_key(field) {
                                diagnostics.push(diag(
                                    DIAG_MISSING_REQUIRED_FIELD,
                                    if authoritative_contact {
                                        format!("missing required field `{field}`")
                                    } else {
                                        "table insertion omits a required field".into()
                                    },
                                ));
                            }
                        }
                    } else if upsert {
                        // Upsert is a patch when its key identifies an
                        // existing row.  The runtime validates completeness
                        // if the key is absent, so semantic admission only
                        // requires key fields needed to select that row.
                        for field in &schema.required {
                            if schema.keys.iter().any(|(key, _)| key == field)
                                && !supplied.contains_key(field)
                            {
                                diagnostics.push(diag(
                                    DIAG_MISSING_REQUIRED_FIELD,
                                    if authoritative_contact {
                                        format!("missing required field `{field}`")
                                    } else {
                                        "table upsert omits a required primary key field".into()
                                    },
                                ));
                            }
                        }
                    }
                    if !supplied.is_empty() {
                        for field in &schema.computed {
                            if supplied.contains_key(field) {
                                diagnostics.push(diag(
                                    if insertion_like {
                                        DIAG_COMPUTED_INSERT
                                    } else {
                                        DIAG_COMPUTED_DIRECT_WRITE
                                    },
                                    if authoritative_contact {
                                        if insertion_like {
                                            format!(
                                                "computed field `{field}` cannot be supplied during insert"
                                            )
                                        } else {
                                            format!(
                                                "computed field `{field}` cannot be updated directly"
                                            )
                                        }
                                    } else if insertion_like {
                                        "table insertion cannot supply a computed field".into()
                                    } else {
                                        "table update cannot change a computed field".into()
                                    },
                                ));
                            }
                        }
                    }
                    if update
                        && !supplied.is_empty()
                        && schema
                            .keys
                            .iter()
                            .any(|(field, _)| supplied.contains_key(field))
                    {
                        diagnostics
                            .push(diag(
                                DIAG_PRIMARY_KEY_IMMUTABLE,
                                if authoritative_contact {
                                    "primary keys are immutable through update"
                                } else {
                                    "table update cannot change a primary key; use rekey"
                                },
                            ));
                    }
                }
                inferred
            })
            .unwrap_or_else(|| infer(&argument.value, scope, local, diagnostics))
        } else if key_argument && let Some(schema) = admission {
            let expected = match schema.keys.as_slice() {
                [(_, ty)] => ty.clone(),
                keys => Type::Tuple(keys.iter().map(|(_, ty)| ty.clone()).collect()),
            };
            let inferred = infer_contextual(&argument.value, &expected, scope, local, diagnostics);
            require_same(&expected, &inferred.ty, diagnostics);
            inferred
        } else {
            infer(&argument.value, scope, local, diagnostics)
        };
        effects.join(&inferred.effects);
    }
    Some(Inferred {
        ty: result,
        effects,
    })
}

/// Check provided write fields against the known row schema. Required/defaulted
/// and computed-field admission needs declaration metadata beyond the row type.
pub(crate) fn infer_table_row_input(
    expression: &Expr,
    fields: &BTreeMap<String, Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
    source_blob_values_are_external: bool,
) -> Inferred {
    let inferred = if let Expr::Record {
        fields: supplied, ..
    } = expression
    {
        let mut actual = BTreeMap::new();
        let mut effects = EffectSummary::default();
        for field in supplied {
            let value = if let Some(expected) = fields.get(&field.name) {
                if source_blob_values_are_external
                    && matches!(expected, Type::Named(name) if matches!(name.as_str(), "Blob" | "std.BINARY_LARGE_OBJECT"))
                {
                    Inferred::pure(expected.clone())
                } else {
                    infer_contextual(&field.value, expected, scope, local, diagnostics)
                }
            } else {
                infer(&field.value, scope, local, diagnostics)
            };
            effects.join(&value.effects);
            actual.insert(field.name.clone(), value.ty);
        }
        Inferred {
            ty: Type::Record(actual),
            effects,
        }
    } else {
        infer(expression, scope, local, diagnostics)
    };
    if let Type::Record(actual) = &inferred.ty {
        for (name, actual) in actual {
            match fields.get(name) {
                None => diagnostics.push(diag(
                    DIAG_TABLE_UNKNOWN_FIELD,
                    format!(
                        "table write contains an unknown field `{}`",
                        if diagnostic_identifier(name) {
                            name.as_str()
                        } else {
                            "<field>"
                        }
                    ),
                )),
                Some(expected) if !types_match(expected, actual) => {
                    diagnostics.push(diag(
                        DIAG_TABLE_FIELD_TYPE,
                        format!(
                            "table write field has an incompatible type: expected {}, found {}",
                            diagnostic_type_name(expected),
                            diagnostic_type_name(actual),
                        ),
                    ));
                }
                _ => {}
            }
        }
    } else if !matches!(inferred.ty, Type::Error) {
        diagnostics.push(diag(DIAG_TYPE, "table write requires a record"));
    }
    inferred
}

/// Describe static types without serializing values or caller-supplied paths.
pub(crate) fn diagnostic_type_name(ty: &Type) -> String {
    match ty {
        Type::Int => "Int".into(),
        Type::Decimal => "Decimal".into(),
        Type::Float => "Float".into(),
        Type::Date => "Date".into(),
        Type::Instant => "Instant".into(),
        Type::Text => "Str".into(),
        Type::Bool => "Bool".into(),
        Type::Null => "Null".into(),
        Type::Unit => "Unit".into(),
        Type::List(element) => format!("[{}]", diagnostic_type_name(element)),
        Type::Optional(element) => format!("{}?", diagnostic_type_name(element)),
        Type::Named(name) if diagnostic_identifier(name) => name.clone(),
        Type::Applied { base, arguments } if diagnostic_identifier(base) => format!(
            "{base}<{}>",
            arguments
                .iter()
                .map(diagnostic_type_name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => "<type>".into(),
    }
}

pub(crate) fn diagnostic_identifier(name: &str) -> bool {
    name.split('.').all(|part| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

pub(crate) fn table_symbol<'a>(
    expr: &Expr,
    scope: &'a Scope,
    local: &'a BTreeMap<String, Symbol>,
) -> Option<&'a Symbol> {
    match expr {
        Expr::Name { text, .. } => local
            .get(text)
            .or_else(|| scope.names.get(text))
            .filter(|symbol| symbol.kind == SymbolKind::Table),
        Expr::Field { .. } => {
            let path = qualified_path(expr)?;
            if local.contains_key(path[0]) {
                return None;
            }
            qualified_module_symbol(&path, scope).filter(|symbol| symbol.kind == SymbolKind::Table)
        }
        _ => None,
    }
}

/// The qualified identity of the table a source expression names: the key of
/// `Scope::table_rows` and of every table dependency, never its spelling.
pub(crate) fn table_identity(
    expr: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<String> {
    match &table_symbol(expr, scope, local)?.ty {
        Type::Named(identity) => Some(identity.clone()),
        _ => None,
    }
}

/// Names the module that owns the member `path.last()` of a qualified source
/// path, or `None` when the path does not start at a module.
///
/// A `use` binding of the first segment takes precedence (ORNA-IMPORT-001,
/// ORNA-IMPORT-002); its remaining segments before the member name nested
/// namespaces, so the result may name an absent module. Otherwise, when the
/// first segment is not a declared or imported name (ORNA-IMPORT-007), the
/// segments before the member must spell a reachable module's full namespace
/// path (ORNA-NS-001, ORNA-NS-002; grammar `qualified_name`):
/// `mail.google.sync` names `sync` in `mail/google.orna` with or without
/// `use mail.google;`. The reserved roots keep their dedicated resolution.
pub(crate) fn module_member_namespace(path: &[&str], scope: &Scope) -> Option<Namespace> {
    let (first, rest) = path.split_first()?;
    let (_, intermediate) = rest.split_last()?;
    if let Some(root) = scope.modules.get(*first) {
        let mut namespace = root.clone();
        namespace
            .0
            .extend(intermediate.iter().map(|part| (*part).to_owned()));
        return Some(namespace);
    }
    if matches!(*first, "std" | "sys")
        || scope.names.contains_key(*first)
        || scope.ambiguous.contains(*first)
    {
        return None;
    }
    let namespace = Namespace(
        path[..path.len() - 1]
            .iter()
            .map(|part| (*part).to_owned())
            .collect(),
    );
    scope
        .available_modules
        .contains_key(&namespace)
        .then_some(namespace)
}

pub(crate) fn qualified_module_symbol<'a>(path: &[&str], scope: &'a Scope) -> Option<&'a Symbol> {
    let namespace = module_member_namespace(path, scope)?;
    scope
        .available_modules
        .get(&namespace)
        .and_then(|module| module.exports.get(*path.last()?))
}

pub(crate) fn infer_table_projection(
    expression: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<Inferred> {
    let Expr::Field { base, name, .. } = expression else {
        return None;
    };
    let table = table_symbol(base, scope, local)?;
    let fields = table.table_fields().or_else(|| match &table.ty {
        Type::Record(fields) => Some(fields),
        Type::Named(table_name) => match scope.table_rows.get(table_name)? {
            Type::Record(fields) => Some(fields),
            _ => None,
        },
        _ => None,
    })?;
    let field = fields.get(name)?.clone();
    Some(Inferred::pure(function(vec![table.ty.clone()], field)))
}

/// Resolves a member through an explicitly imported module root or a
/// reachable module's full namespace path (see `module_member_namespace`).
/// Intermediate path segments are namespaces, not values, so arbitrary
/// nominal/record values remain governed by ordinary field inference.
pub(crate) fn resolve_qualified_module_member(
    path: &[&str],
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let symbol = qualified_module_symbol(path, scope);
    match symbol {
        Some(symbol) => Inferred {
            ty: symbol.ty.clone(),
            effects: symbol.effects.clone(),
        },
        None => {
            // The note names the module the member was looked up in: its canonical
            // namespace, so an alias such as `query_ops` reports `std.query`.
            let module = module_member_namespace(path, scope).map_or_else(
                || path[..path.len() - 1].join("."),
                |namespace| namespace.display(),
            );
            diagnostics.push(
                diag(DIAG_UNRESOLVED, "module member cannot be resolved").with_note(
                    SafeText::new(format!(
                        "{UNRESOLVED_MEMBER_NOTE}{module}.{}",
                        path[path.len() - 1]
                    ))
                    .expect("identifier text is safe"),
                ),
            );
            Inferred::error()
        }
    }
}

pub(crate) fn require_same(expected: &Type, actual: &Type, diagnostics: &mut Vec<Diagnostic>) {
    if !types_match(expected, actual)
        && !matches!(expected, Type::Error)
        && !matches!(actual, Type::Error)
    {
        diagnostics.push(type_mismatch(
            DIAG_TYPE,
            "static types are incompatible",
            expected,
            actual,
        ));
    }
}

/// Like `require_same`, but used only at function-return checks. It reports
/// `ORNA091-E-CONVERSION-CHAIN` when the actual nominal type reaches the
/// expected one only through a multi-step `From` chain. ORNA-CONVERT-007
/// names no code; `ORNA091-E-CONVERSION-CHAIN` is a project-local code. A
/// plain mismatch stays `ORNA-S021-TYPE`. Let, argument and field sites use
/// `require_same` and do not get this code.
pub(crate) fn require_same_in(
    scope: &Scope,
    expected: &Type,
    actual: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let (Type::Named(target), Type::Named(source)) = (expected, actual)
        && target != source
        && !types_match(expected, actual)
        && has_multi_step_conversion(scope, target, source)
    {
        diagnostics.push(type_mismatch(
            DIAG_CONVERSION_CHAIN,
            "implicit conversion chains are not searched; name each conversion explicitly",
            expected,
            actual,
        ));
        return;
    }
    require_same(expected, actual, diagnostics);
}

/// True when `source` is reachable from `target` by following `From` edges
/// through at least one intermediate nominal type.
pub(crate) fn has_multi_step_conversion(scope: &Scope, target: &str, source: &str) -> bool {
    let mut seen = BTreeSet::new();
    // Start from the sources of each direct source: `source` must be reached
    // at depth two or more, so a single direct `From` is not a chain.
    let mut pending: Vec<&str> = scope
        .nominal_conversions
        .get(target)
        .into_iter()
        .flatten()
        .filter_map(|ty| match ty {
            Type::Named(name) => scope.nominal_conversions.get(name.as_str()),
            _ => None,
        })
        .flatten()
        .filter_map(|ty| match ty {
            Type::Named(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if current == source {
            return true;
        }
        if let Some(next) = scope.nominal_conversions.get(current) {
            pending.extend(next.iter().filter_map(|ty| match ty {
                Type::Named(name) => Some(name.as_str()),
                _ => None,
            }));
        }
    }
    false
}

/// A static type mismatch that names both types, so the person can see which
/// side has which type. The message stays stable; the types ride in a note.
pub(crate) fn type_mismatch(
    code: &'static str,
    message: &str,
    expected: &Type,
    actual: &Type,
) -> Diagnostic {
    diag(code, message).with_note(
        SafeText::new(format!(
            "expected {}, found {}",
            type_label(expected),
            type_label(actual)
        ))
        .expect("valid diagnostic note"),
    )
}

/// Spells a static type the way source writes it.
pub(crate) fn type_label(ty: &Type) -> String {
    match ty {
        Type::Int => "Int".into(),
        Type::Decimal => "Decimal".into(),
        Type::Float => "Float".into(),
        Type::Date => "Date".into(),
        Type::Instant => "Instant".into(),
        Type::Text => "Text".into(),
        Type::Bool => "Bool".into(),
        Type::Null => "Null".into(),
        Type::Unit => "Unit".into(),
        Type::List(element) => format!("List<{}>", type_label(element)),
        Type::Range(element) => format!("Range<{}>", type_label(element)),
        Type::Relation(element) => format!("Relation<{}>", type_label(element)),
        Type::Stream(element) => format!("Stream<{}>", type_label(element)),
        Type::Optional(inner) => format!("Optional<{}>", type_label(inner)),
        Type::Tuple(items) => format!(
            "({})",
            items.iter().map(type_label).collect::<Vec<_>>().join(", ")
        ),
        Type::Record(_) => "record".into(),
        Type::Applied { base, arguments } if arguments.is_empty() => base.clone(),
        Type::Applied { base, arguments } => format!(
            "{base}<{}>",
            arguments
                .iter()
                .map(type_label)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Quantity { .. } => "quantity".into(),
        Type::MoneyPerUnit { .. } => "money rate".into(),
        Type::Function { .. } => "function".into(),
        Type::Named(name) => name.clone(),
        Type::Bottom => "no value".into(),
        Type::Error => "unknown type".into(),
    }
}

pub(crate) fn types_match(expected: &Type, actual: &Type) -> bool {
    if !checkpoint_snapshot_maps_are_valid(expected) || !checkpoint_snapshot_maps_are_valid(actual)
    {
        return false;
    }
    if expected == actual {
        return checkpoint_snapshot_maps_are_valid(expected);
    }
    if matches!(actual, Type::Bottom) {
        return true;
    }
    if matches!(expected, Type::Bottom) {
        return false;
    }
    if matches!(expected, Type::Error) || matches!(actual, Type::Error) {
        return true;
    }
    // A shadowing lambda's contextual parameter key identifies its binder in
    // returned callback types; callers may still supply any SnapshotRef value.
    if snapshot_ref_context_key(expected)
        .and_then(snapshot_ref_binder_id)
        .is_some()
        && is_snapshot_ref_value(actual)
    {
        return true;
    }
    if expected == &Type::Named("sys.SnapshotRef".into()) && is_contextual_snapshot_ref(actual) {
        return true;
    }
    match (expected, actual) {
        (Type::Optional(_), Type::Null) => true,
        (Type::List(expected), Type::List(actual))
        | (Type::Range(expected), Type::Range(actual))
        | (Type::Relation(expected), Type::Relation(actual))
        | (Type::Stream(expected), Type::Stream(actual)) => types_match(expected, actual),
        // Optional parameters accept their non-null payload directly; callers
        // do not need to wrap ordinary values in a nullable constructor.
        (Type::Optional(expected), actual) => types_match(expected, actual),
        // Tuple parameters bind their component types recursively, including
        // contextual SnapshotRef leaves.
        (Type::Tuple(expected), Type::Tuple(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| types_match(expected, actual))
        }
        (
            Type::Applied {
                base: expected_base,
                arguments: expected_arguments,
            },
            Type::Applied {
                base: actual_base,
                arguments: actual_arguments,
            },
        ) if expected_base == "Money"
            && actual_base == "Money"
            && expected_arguments.len() == 1
            && actual_arguments.len() == 1
            && expected_arguments.first() == Some(&Type::Error) =>
        {
            true
        }
        (
            Type::Function {
                parameters: expected_parameters,
                default_parameters: expected_defaults,
                result: expected_result,
                ..
            },
            Type::Function {
                parameters: actual_parameters,
                default_parameters: actual_defaults,
                result: actual_result,
                ..
            },
        ) => {
            expected_parameters.len() == actual_parameters.len()
                && expected_defaults == actual_defaults
                && expected_parameters
                    .iter()
                    .zip(actual_parameters)
                    .all(|(expected, actual)| types_match(expected, actual))
                && types_match(expected_result, actual_result)
        }
        _ => false,
    }
}

/// `types_match` intentionally treats `Error` as compatible to avoid
/// cascading diagnostics. Tuple pin specialization must instead fail closed
/// when recovery left an invalid component, or valid siblings could leak a
/// partial set of capture identities into a later wave.
pub(crate) fn contains_type_error(ty: &Type) -> bool {
    match ty {
        Type::Error => true,
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => contains_type_error(element),
        Type::Record(fields) => fields.values().any(contains_type_error),
        Type::Tuple(elements) => elements.iter().any(contains_type_error),
        Type::Applied { arguments, .. } => arguments.iter().any(contains_type_error),
        Type::Quantity { numeric, .. } => contains_type_error(numeric),
        Type::MoneyPerUnit { currency, unit } => {
            contains_type_error(currency) || contains_type_error(unit)
        }
        Type::Function {
            parameters, result, ..
        } => parameters.iter().any(contains_type_error) || contains_type_error(result),
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Named(_)
        | Type::Bottom => false,
    }
}

/// Tuple widths in callback signatures are part of the binding wave even when
/// the tuple is nested under a higher-order function or aggregate parameter.
/// If a nested width is unknown during recovery, do not partially promote pins
/// from sibling arguments.
pub(crate) fn contains_tuple_type(ty: &Type) -> bool {
    match ty {
        Type::Tuple(_) => true,
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => contains_tuple_type(element),
        Type::Record(fields) => fields.values().any(contains_tuple_type),
        Type::Applied { arguments, .. } => arguments.iter().any(contains_tuple_type),
        Type::Quantity { numeric, .. } => contains_tuple_type(numeric),
        Type::Function {
            parameters, result, ..
        } => parameters.iter().any(contains_tuple_type) || contains_tuple_type(result),
        Type::MoneyPerUnit { currency, unit } => {
            contains_tuple_type(currency) || contains_tuple_type(unit)
        }
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Named(_)
        | Type::Bottom
        | Type::Error => false,
    }
}

/// `Bottom` is compatible during ordinary contextual checking, but a value
/// containing a non-returning tuple or record component is never produced as
/// an argument. Walk only value aggregates: a callback's Bottom result or an
/// empty collection's element type does not make that value incomplete.
pub(crate) fn contains_nonreturning_aggregate_component(ty: &Type) -> bool {
    match ty {
        Type::Bottom => true,
        Type::Record(fields) => fields
            .values()
            .any(contains_nonreturning_aggregate_component),
        Type::Tuple(elements) => elements
            .iter()
            .any(contains_nonreturning_aggregate_component),
        _ => false,
    }
}

pub(crate) fn table_row_types_match(expected: &Type, actual: &Type, scope: &Scope) -> bool {
    let (Type::Named(expected), Type::Named(actual)) = (expected, actual) else {
        return false;
    };
    let same_attached_contact = matches!(
        (expected.as_str(), actual.as_str()),
        ("Contact", "contacts.Contact") | ("contacts.Contact", "Contact")
    );
    matches!(
        (scope.table_rows.get(expected), scope.table_rows.get(actual)),
        (Some(Type::Record(expected)), Some(Type::Record(actual)))
            if same_attached_contact && expected == actual
    )
}

pub(crate) fn intrinsic_value_type(name: &str) -> Option<Type> {
    match name {
        "now" => Some(function(Vec::new(), Type::Instant)),
        "uuid7" => Some(function(Vec::new(), Type::Named("std.UUID".into()))),
        "log" => Some(function(vec![Type::Text], Type::Null)),
        "parallel" => Some(function(
            vec![Type::List(Box::new(Type::Error))],
            Type::List(Box::new(Type::Error)),
        )),
        "merge_streams" => Some(function(
            vec![Type::List(Box::new(Type::Error))],
            Type::Stream(Box::new(Type::Error)),
        )),
        "half_even" => Some(Type::Named("std.Rounding".into())),
        "CWD" | "HEAD" => Some(contextual_snapshot_ref(&format!("selector:{name}"))),
        _ => None,
    }
}

pub(crate) fn infer_parallel_call(
    callee: &Expr,
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let is_parallel = qualified_path(callee)
        .as_deref()
        .is_some_and(|path| path == ["std", "concurrent", "parallel"])
        || matches!(callee, Expr::Name { text, .. }
            if text == "parallel"
                && local.get(text).or_else(|| scope.names.get(text)).is_none());
    if !is_parallel || arguments.len() != 1 || arguments[0].name.is_some() {
        return None;
    }
    let Expr::List { elements, .. } = &arguments[0].value else {
        return None;
    };

    let mut effects = EffectSummary::default();
    let mut result_type: Option<Type> = None;
    for callback in elements {
        let inferred = infer(callback, scope, local, diagnostics);
        effects.join(&inferred.effects);
        let Type::Function { result, .. } = inferred.ty else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "parallel entries must be callback functions",
            ));
            continue;
        };
        if let Some(expected) = &result_type {
            if !types_match(expected, &result) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "parallel callbacks must have compatible result types",
                ));
            }
        } else {
            result_type = Some(*result);
        }
    }
    Some(Inferred {
        ty: Type::Stream(Box::new(result_type.unwrap_or(Type::Error))),
        effects,
    })
}

pub(crate) fn infer_race_call(
    callee: &Expr,
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let is_race = qualified_path(callee)
        .as_deref()
        .is_some_and(|path| path == ["std", "concurrent", "race"])
        || matches!(callee, Expr::Name { text, .. }
            if text == "race"
                && local.get(text).or_else(|| scope.names.get(text)).is_none());
    if !is_race || arguments.len() != 1 || arguments[0].name.is_some() {
        return None;
    }
    let Expr::List { elements, .. } = &arguments[0].value else {
        return None;
    };

    let mut effects = EffectSummary::default();
    let mut result_type: Option<Type> = None;
    for callback in elements {
        let inferred = infer(callback, scope, local, diagnostics);
        effects.join(&inferred.effects);
        let Type::Function { result, .. } = inferred.ty else {
            diagnostics.push(diag(DIAG_TYPE, "race entries must be callback functions"));
            continue;
        };
        if let Some(expected) = &result_type {
            if !types_match(expected, &result) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "race callbacks must have compatible result types",
                ));
            }
        } else {
            result_type = Some(*result);
        }
    }
    Some(Inferred {
        ty: result_type.unwrap_or(Type::Error),
        effects,
    })
}

pub(crate) fn infer_timeout_call(
    callee: &Expr,
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let is_timeout = qualified_path(callee)
        .as_deref()
        .is_some_and(|path| path == ["std", "concurrent", "timeout"])
        || matches!(callee, Expr::Name { text, .. }
            if text == "timeout"
                && local.get(text).or_else(|| scope.names.get(text)).is_none());
    if !is_timeout {
        return None;
    }

    let mut callback = None;
    let mut duration = None;
    let mut positional = 0;
    for argument in arguments {
        match argument.name.as_deref() {
            Some("callback") if callback.is_none() => callback = Some(&argument.value),
            Some("duration") if duration.is_none() => duration = Some(&argument.value),
            Some(_) => {
                diagnostics.push(diag(DIAG_TYPE, "timeout expects a callback and a duration"));
                return Some(Inferred::error());
            }
            None => {
                let slot = match positional {
                    0 => &mut callback,
                    1 => &mut duration,
                    _ => {
                        diagnostics
                            .push(diag(DIAG_TYPE, "timeout expects a callback and a duration"));
                        return Some(Inferred::error());
                    }
                };
                if slot.is_some() {
                    diagnostics.push(diag(DIAG_TYPE, "timeout expects a callback and a duration"));
                    return Some(Inferred::error());
                }
                *slot = Some(&argument.value);
                positional += 1;
            }
        }
    }
    let (Some(callback), Some(duration)) = (callback, duration) else {
        diagnostics.push(diag(DIAG_TYPE, "timeout expects a callback and a duration"));
        return Some(Inferred::error());
    };

    let callback = infer(callback, scope, local, diagnostics);
    let duration = infer(duration, scope, local, diagnostics);
    let mut effects = callback.effects.clone();
    effects.join(&duration.effects);
    let Type::Function {
        parameters, result, ..
    } = callback.ty
    else {
        diagnostics.push(diag(DIAG_TYPE, "timeout callback must be a function"));
        return Some(Inferred::error_with(effects));
    };
    if !parameters.is_empty() {
        diagnostics.push(diag(DIAG_TYPE, "timeout callback must not take parameters"));
    }
    let expected_duration = Type::Named("std.DURATION".into());
    if !types_match(&expected_duration, &duration.ty) {
        diagnostics.push(diag(DIAG_TYPE, "timeout duration must be a Duration"));
    }
    Some(Inferred {
        ty: *result,
        effects,
    })
}
