//! Intrinsic result predicates shared by the registry typing engine and the
//! quantity-aware statistics rules.

use std::collections::BTreeMap;

use orna_syntax::Expr;
use orna_value::Diagnostic;

use super::*;

/// Values a stable ascending `sort_by` key may produce. Float uses the
/// canonical total order (`spec 09:32`).
pub(crate) fn is_sort_key_type(ty: &Type) -> bool {
    match ty {
        Type::Error
        | Type::Bottom
        | Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Named(_)
        | Type::Applied { .. } => true,
        Type::Quantity { numeric, .. } => is_sort_key_type(numeric),
        Type::Range(element) => is_sort_key_type(element),
        Type::Tuple(elements) => elements.iter().all(is_sort_key_type),
        Type::List(_)
        | Type::Relation(_)
        | Type::Stream(_)
        | Type::Record(_)
        | Type::Optional(_)
        | Type::Null
        | Type::Unit
        | Type::MoneyPerUnit { .. }
        | Type::Function { .. } => false,
    }
}

/// `sum` admits money and numeric quantities, but not absolute affine values
/// (ORNA-UNIT-007).
pub(crate) fn is_sum_element_type(element: &Type) -> bool {
    !is_absolute_affine(element)
        && (matches!(element, Type::Applied { base, .. } if base == "Money")
            || is_statistics_numeric_type(element))
}

/// Infers the rows argument of a finite-list aggregate, giving an empty list
/// literal the element type the surrounding context supplies instead of
/// falling back to `Int` (`spec 09:34`, ORNA-UNIT-007).
pub(crate) fn infer_finite_list_aggregate_rows(
    expression: &Expr,
    empty_element: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Expr::Group { inner, .. } = expression
        && is_empty_list_literal(expression)
    {
        return infer_finite_list_aggregate_rows(inner, empty_element, scope, local, diagnostics);
    }
    if is_empty_list_literal(expression) {
        return infer_contextual(
            expression,
            &Type::List(Box::new(empty_element.cloned().unwrap_or(Type::Int))),
            scope,
            local,
            diagnostics,
        );
    }
    infer(expression, scope, local, diagnostics)
}

/// Types a finite-list `one`, `every` or `exists` predicate. The operation
/// keeps its own shape diagnostics because these predicates have a narrower
/// contract than a general collection callback.
pub(crate) fn infer_finite_list_predicate(
    operation: &str,
    expression: &Expr,
    parameter: Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Expr::Lambda {
        parameters, body, ..
    } = expression
    {
        let [lambda_parameter] = parameters.as_slice() else {
            diagnostics.push(diag(
                DIAG_TYPE,
                format!("finite-list {operation} predicate must take one parameter"),
            ));
            return Inferred::error();
        };
        if let Some(annotation) = &lambda_parameter.annotation {
            require_same(
                &parameter,
                &resolved_type_of(annotation, scope),
                diagnostics,
            );
        }
        let Pattern::Name(name, _) = &lambda_parameter.pattern else {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                format!("finite-list {operation} predicate pattern is outside this semantic slice"),
            ));
            return Inferred::error();
        };
        let mut callback_locals = local.clone();
        insert_local_binding(name, parameter, &mut callback_locals, diagnostics);
        let inferred = infer_function_body(body, None, scope, &callback_locals, diagnostics);
        require_same(&Type::Bool, &inferred.ty, diagnostics);
        return Inferred {
            ty: inferred.ty,
            effects: inferred.effects,
        };
    }

    let callback = infer(expression, scope, local, diagnostics);
    let Type::Function {
        parameters, result, ..
    } = callback.ty
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("finite-list {operation} predicate must be a one-parameter function"),
        ));
        return Inferred::error_with(callback.effects);
    };
    if parameters.len() != 1 {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("finite-list {operation} predicate must take one parameter"),
        ));
    } else {
        require_same(&parameter, &parameters[0], diagnostics);
    }
    require_same(&Type::Bool, &result, diagnostics);
    Inferred {
        ty: *result,
        effects: callback.effects,
    }
}

pub(crate) fn is_empty_list_literal(expression: &Expr) -> bool {
    match expression {
        Expr::List { elements, .. } => elements.is_empty(),
        Expr::Group { inner, .. } => is_empty_list_literal(inner),
        _ => false,
    }
}
