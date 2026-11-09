//! Finite-list and relation helper calls.

use std::collections::BTreeMap;

use orna_foundation::Diagnostic;
use orna_syntax::{Expr, Pattern};

use super::*;

/// Types the operation's result from its bound argument values, checking each
/// operation's own constraints.
pub(crate) fn finite_list_helper_result(
    call: FiniteListCall<'_>,
    effects: &mut EffectSummary,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    let FiniteListCall {
        operation,
        values,
        slots,
        arguments,
        scope,
        local,
    } = call;
    let relation_input = matches!(
        values.first().and_then(Option::as_ref),
        Some(Type::Relation(_))
    );
    let container = |element: Type| {
        if relation_input {
            Type::Relation(Box::new(element))
        } else {
            Type::List(Box::new(element))
        }
    };
    let list_element = |index: usize, name: &str, diagnostics: &mut Vec<Diagnostic>| match values
        .get(index)
        .and_then(Option::as_ref)
    {
        Some(Type::List(element) | Type::Relation(element)) => element.as_ref().clone(),
        Some(Type::Error) | None => Type::Error,
        Some(_) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                format!("std.collection {operation} {name} must be a finite list"),
            ));
            Type::Error
        }
    };
    let first = list_element(0, "input", diagnostics);
    match operation {
        "chunk" => {
            let size = values
                .get(1)
                .and_then(Option::as_ref)
                .unwrap_or(&Type::Error);
            require_same(&Type::Int, size, diagnostics);
            if let Some(index) = slots.get(1).and_then(|slot| *slot)
                && is_non_positive_integer_constant(&arguments[index].value)
            {
                diagnostics.push(diag(DIAG_TYPE, "chunk size must be positive"));
            }
            container(Type::List(Box::new(first)))
        }
        "flatten" => match first {
            Type::List(inner) => container(*inner),
            Type::Error => Type::Error,
            _ => {
                diagnostics.push(diag(DIAG_TYPE, "flatten requires a list of finite lists"));
                Type::Error
            }
        },
        "partition" | "split_when" => {
            if let Some(index) = slots.get(1).and_then(|slot| *slot) {
                let callback = infer_finite_list_callback(
                    &arguments[index].value,
                    first.clone(),
                    Type::Bool,
                    scope,
                    local,
                    diagnostics,
                );
                effects.join(&callback.effects);
            }
            if operation == "partition" {
                Type::Tuple(vec![container(first.clone()), container(first)])
            } else {
                container(Type::List(Box::new(first)))
            }
        }
        "zip" | "zip_exact" => {
            let right_is_relation = matches!(
                values.get(1).and_then(Option::as_ref),
                Some(Type::Relation(_))
            );
            if right_is_relation != relation_input {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("std.collection {operation} inputs must use the same collection kind"),
                ));
            }
            let right = list_element(1, "right input", diagnostics);
            if operation == "zip_exact" {
                effects.may_fail = true;
            }
            container(Type::Tuple(vec![first, right]))
        }
        "unique" => {
            if is_default_float_equality_type(&first) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "unique requires lawful equality; default Float equality is unavailable",
                ));
                Type::Error
            } else {
                container(first)
            }
        }
        "pairs" => container(Type::Tuple(vec![first.clone(), first])),
        "group_by" | "rank" => {
            let key_type = if let Some(index) = slots.get(1).and_then(|slot| *slot) {
                let callback = infer_finite_list_callback(
                    &arguments[index].value,
                    first.clone(),
                    Type::Error,
                    scope,
                    local,
                    diagnostics,
                );
                effects.join(&callback.effects);
                callback.ty
            } else {
                Type::Error
            };
            if key_type != Type::Error && !is_evaluator_ordered_comparison_type(&key_type) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("std.collection {operation} key must have a lawful total order"),
                ));
            }
            if operation == "group_by" {
                container(Type::Tuple(vec![key_type, Type::List(Box::new(first))]))
            } else {
                container(Type::Tuple(vec![first, Type::Int]))
            }
        }
        "window" => {
            for index in [1, 2] {
                let size = values
                    .get(index)
                    .and_then(Option::as_ref)
                    .unwrap_or(&Type::Error);
                require_same(&Type::Int, size, diagnostics);
                if let Some(argument_index) = slots.get(index).and_then(|slot| *slot)
                    && is_non_positive_integer_constant(&arguments[argument_index].value)
                {
                    diagnostics.push(diag(DIAG_TYPE, "window size and step must be positive"));
                }
            }
            container(Type::List(Box::new(first)))
        }
        "asof_join" => {
            let right = list_element(1, "right input", diagnostics);
            require_same(&first, &right, diagnostics);
            for index in [2, 3] {
                if let Some(argument_index) = slots.get(index).and_then(|slot| *slot) {
                    let callback = infer_finite_list_callback(
                        &arguments[argument_index].value,
                        first.clone(),
                        Type::Error,
                        scope,
                        local,
                        diagnostics,
                    );
                    effects.join(&callback.effects);
                    if index == 2
                        && callback.ty != Type::Error
                        && !is_evaluator_ordered_comparison_type(&callback.ty)
                    {
                        diagnostics.push(diag(DIAG_TYPE, "asof_join time key must be ordered"));
                    }
                }
            }
            let right_is_relation = matches!(
                values.get(1).and_then(Option::as_ref),
                Some(Type::Relation(_))
            );
            if right_is_relation != relation_input {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "std.collection asof_join inputs must use the same collection kind",
                ));
            }
            container(Type::Tuple(vec![
                first.clone(),
                Type::Optional(Box::new(first)),
            ]))
        }
        "bucket_by" => {
            let time_shape = matches!(&first, Type::Instant)
                || matches!(&first, Type::Record(fields) if fields.get("time") == Some(&Type::Instant));
            if !time_shape && first != Type::Error {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "bucket_by requires Instant values or rows with an Instant time field",
                ));
            }
            container(Type::List(Box::new(first)))
        }
        _ => unreachable!("finite-list helper was checked"),
    }
}

pub(crate) fn infer_finite_list_simple_collection(
    operation: &str,
    input: Option<Inferred>,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let pipeline = input.is_some();
    let expected: &[&str] = match operation {
        "distinct" | "count" => &["rows"],
        "take" | "drop" => &["rows", "count"],
        "union" => &["left", "right"],
        _ => unreachable!("simple finite-list operation was checked"),
    };
    let mut slots = vec![None; expected.len()];
    let mut malformed = false;
    let mut positional = usize::from(pipeline);
    let mut named_started = false;
    for (index, argument) in arguments.iter().enumerate() {
        let slot = match argument.name.as_deref() {
            Some(name) => {
                named_started = true;
                expected.iter().position(|expected| *expected == name)
            }
            None if named_started || positional >= expected.len() => None,
            None => {
                let slot = positional;
                positional += 1;
                Some(slot)
            }
        };
        if let Some(slot) = slot {
            if pipeline && slot == 0 || slots[slot].replace(index).is_some() {
                malformed = true;
            }
        } else {
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
    if slots.iter().skip(1).any(Option::is_none) {
        malformed = true;
    }

    let mut effects = input
        .as_ref()
        .map(|input| input.effects.clone())
        .unwrap_or_default();
    let mut values = vec![None; expected.len()];
    if let Some(input) = input {
        values[0] = Some(input.ty);
    }
    for (index, argument) in arguments.iter().enumerate() {
        let Some(slot) = slots.iter().position(|candidate| *candidate == Some(index)) else {
            let inferred = infer(&argument.value, scope, local, diagnostics);
            effects.join(&inferred.effects);
            continue;
        };
        let inferred = infer(&argument.value, scope, local, diagnostics);
        effects.join(&inferred.effects);
        values[slot] = Some(inferred.ty);
    }
    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("std.collection {operation} arguments do not match its static signature"),
        ));
    }
    let first = values
        .first()
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or(Type::Error);
    let Type::List(element) = first else {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("finite-list {operation} requires a finite list"),
        ));
        return Inferred::error_with(effects);
    };
    let result = match operation {
        "count" => Type::Int,
        "distinct" => {
            if is_default_float_equality_type(&element) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "distinct requires a lawful equality key; default Float equality is unavailable",
                ));
                Type::Error
            } else {
                Type::List(element)
            }
        }
        "take" | "drop" => {
            let count = values
                .get(1)
                .and_then(Option::as_ref)
                .unwrap_or(&Type::Error);
            if count != &Type::Int && count != &Type::Error {
                diagnostics.push(diag(DIAG_TYPE, format!("{operation} count must be an Int")));
                Type::Error
            } else if count == &Type::Int
                && slots[1]
                    .is_some_and(|index| is_negative_integer_constant(&arguments[index].value))
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("{operation} count must be nonnegative"),
                ));
                Type::Error
            } else {
                Type::List(element)
            }
        }
        "union" => {
            let right = values
                .get(1)
                .and_then(Option::as_ref)
                .cloned()
                .unwrap_or(Type::Error);
            match right {
                Type::List(right_element) if types_match(&element, &right_element) => {
                    Type::List(element)
                }
                Type::Error => Type::Error,
                _ => {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "finite-list union requires lists with compatible element types",
                    ));
                    Type::Error
                }
            }
        }
        _ => unreachable!("simple finite-list operation was checked"),
    };
    Inferred {
        ty: if malformed { Type::Error } else { result },
        effects,
    }
}

/// Checks the finite-list `sort_by(rows, key)` signature.  The key callback is
/// evaluated once per source value by the runtime and must produce a value with
/// an available total order; callback effects and failures remain visible to
/// the surrounding expression before any ordering optimisation.
pub(crate) fn infer_finite_list_sort_by(
    arguments: &[orna_syntax::Argument],
    input: Option<Inferred>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
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
            Some("key") => {
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
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
        }
        Type::Error
    };

    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            "finite-list sort_by arguments do not match its static signature",
        ));
    }
    let Type::List(element) = row else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "finite-list sort_by requires a finite list",
        ));
        return Inferred::error_with(effects);
    };
    let Some(index) = slots[1] else {
        return Inferred::error_with(effects);
    };
    let callback = infer_finite_list_sort_key(
        &arguments[index].value,
        element.as_ref().clone(),
        scope,
        local,
        diagnostics,
    );
    effects.join(&callback.effects);
    if !is_sort_key_type(&callback.ty) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "finite-list sort_by key must have a total order",
        ));
    }
    Inferred {
        ty: if !malformed && is_sort_key_type(&callback.ty) {
            Type::List(element)
        } else {
            Type::Error
        },
        effects,
    }
}

pub(crate) fn infer_finite_list_sort_key(
    expression: &Expr,
    parameter: Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Expr::Lambda { parameters, .. } = expression
        && let [lambda_parameter] = parameters.as_slice()
        && let Some(annotation) = &lambda_parameter.annotation
    {
        require_same(
            &parameter,
            &resolved_type_of(annotation, scope),
            diagnostics,
        );
    }
    infer_finite_list_callback(
        expression,
        parameter,
        Type::Error,
        scope,
        local,
        diagnostics,
    )
}

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

/// Checks the bounded finite-list numeric aggregate signatures. The pinned
/// source declarations are generic, so rows supply the element type and the
/// operation admits only supported numeric element kinds. Empty unannotated
/// lists retain the legacy Int-zero rule; input effects are preserved.
pub(crate) fn infer_finite_list_numeric_aggregate(
    operation: &str,
    arguments: &[orna_syntax::Argument],
    input: Option<Inferred>,
    aggregate_element: Option<Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let pipeline = input.is_some();
    let mut row_slot = None;
    let mut malformed = false;
    let mut positional = usize::from(pipeline);
    let mut named_started = false;
    for (index, argument) in arguments.iter().enumerate() {
        let slot = match argument.name.as_deref() {
            Some("rows") => {
                named_started = true;
                Some(0)
            }
            Some(_) => {
                named_started = true;
                malformed = true;
                None
            }
            None if named_started || positional >= 1 => {
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
            && (pipeline && slot == 0 || row_slot.replace(index).is_some())
        {
            malformed = true;
        }
    }
    if pipeline {
        if row_slot.is_some() {
            malformed = true;
        }
    } else if row_slot.is_none() {
        malformed = true;
    }

    let mut effects = input
        .as_ref()
        .map(|input| input.effects.clone())
        .unwrap_or_default();
    let values = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            let value = if row_slot == Some(index) {
                infer_finite_list_aggregate_rows(
                    &argument.value,
                    aggregate_element.as_ref(),
                    scope,
                    local,
                    diagnostics,
                )
            } else {
                infer(&argument.value, scope, local, diagnostics)
            };
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();
    let rows = input
        .map(|input| input.ty)
        .or_else(|| row_slot.and_then(|index| values.get(index).cloned()))
        .unwrap_or(Type::Error);

    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("finite-list {operation} arguments do not match its static signature"),
        ));
    }
    let valid = match (&rows, aggregate_element.as_ref()) {
        (Type::List(element), Some(expected)) => element.as_ref() == expected,
        (Type::List(element), None) if operation == "sum" => is_sum_element_type(element),
        (Type::List(element), None) => is_sort_key_type(element),
        _ => false,
    };
    if !valid {
        if operation == "sum" && matches!(&rows, Type::List(element) if is_absolute_affine(element))
        {
            diagnostics.push(diag(
                DIAG_ABSOLUTE_QUANTITY_SUM,
                format!("cannot {operation} absolute affine quantities"),
            ));
        } else {
            diagnostics.push(diag(
                DIAG_TYPE,
                format!("finite-list {operation} requires a finite list with a supported {operation} element type"),
            ));
        }
    }
    Inferred {
        ty: if !malformed && (valid || aggregate_element.is_some()) {
            let element = aggregate_element
                .or_else(|| match &rows {
                    Type::List(element) => Some(element.as_ref().clone()),
                    _ => None,
                })
                .unwrap_or(Type::Error);
            if operation == "sum" {
                element
            } else {
                Type::Optional(Box::new(element))
            }
        } else {
            Type::Error
        },
        effects,
    }
}

/// `sum` admits money and numeric quantities, but not absolute affine values
/// (ORNA-UNIT-007).
pub(crate) fn is_sum_element_type(element: &Type) -> bool {
    !is_absolute_affine(element)
        && (matches!(element, Type::Applied { base, .. } if base == "Money")
            || is_statistics_numeric_type(element))
}

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

pub(crate) fn is_empty_list_literal(expression: &Expr) -> bool {
    match expression {
        Expr::List { elements, .. } => elements.is_empty(),
        Expr::Group { inner, .. } => is_empty_list_literal(inner),
        _ => false,
    }
}

/// Checks the finite-list `first(rows)` signature. The result is optional
/// independently of whether the input list is known to be empty; runtime
/// evaluation supplies `Some(first)` or `null`.
pub(crate) fn infer_finite_list_first(
    arguments: &[orna_syntax::Argument],
    input: Option<Inferred>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let pipeline = input.is_some();
    let mut row_slot = None;
    let mut malformed = false;
    let mut positional = usize::from(pipeline);
    let mut named_started = false;
    for (index, argument) in arguments.iter().enumerate() {
        let slot = match argument.name.as_deref() {
            Some("rows") => {
                named_started = true;
                Some(0)
            }
            Some(_) => {
                named_started = true;
                malformed = true;
                None
            }
            None if named_started || positional >= 1 => {
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
            && (pipeline && slot == 0 || row_slot.replace(index).is_some())
        {
            malformed = true;
        }
    }
    if !pipeline && row_slot.is_none() {
        malformed = true;
    }

    let mut effects = input
        .as_ref()
        .map(|input| input.effects.clone())
        .unwrap_or_default();
    let values = arguments
        .iter()
        .map(|argument| {
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();
    let rows = input
        .map(|input| input.ty)
        .or_else(|| row_slot.and_then(|index| values.get(index).cloned()))
        .unwrap_or(Type::Error);

    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            "finite-list first arguments do not match its static signature",
        ));
    }
    let Type::List(element) = rows else {
        diagnostics.push(diag(DIAG_TYPE, "finite-list first requires a finite list"));
        return Inferred::error_with(effects);
    };
    if malformed {
        return Inferred::error_with(effects);
    }
    Inferred {
        ty: Type::Optional(element),
        effects,
    }
}

/// Checks the finite-list `one(rows)` and `one(rows, predicate)` overloads.
/// The operation returns the element type directly; exact cardinality and
/// its distinct zero/multiple failure cases belong to runtime evaluation.
pub(crate) fn infer_finite_list_one(
    arguments: &[orna_syntax::Argument],
    input: Option<Inferred>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
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
            Some("predicate") => {
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

    let mut effects = input
        .as_ref()
        .map(|input| input.effects.clone())
        .unwrap_or_default();
    let values = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            if slots[1] == Some(index) && matches!(argument.value, Expr::Lambda { .. }) {
                return Type::Error;
            }
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();
    let rows = input
        .map(|input| input.ty)
        .or_else(|| slots[0].and_then(|index| values.get(index).cloned()))
        .unwrap_or(Type::Error);

    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            "finite-list one arguments do not match its static signature",
        ));
    }
    let Type::List(element) = rows else {
        diagnostics.push(diag(DIAG_TYPE, "finite-list one requires a finite list"));
        return Inferred::error_with(effects);
    };

    if let Some(index) = slots[1] {
        let callback = infer_finite_list_predicate(
            "one",
            &arguments[index].value,
            element.as_ref().clone(),
            scope,
            local,
            diagnostics,
        );
        effects.join(&callback.effects);
    }
    effects.may_fail = true;
    Inferred {
        ty: if malformed { Type::Error } else { *element },
        effects,
    }
}

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

/// Checks the finite-list `every(rows, predicate)` and
/// `exists(rows, predicate)` signatures. Both operations always return Bool;
/// their empty-list identities and short-circuit evaluation belong to the
/// runtime, while callback effects are retained in the semantic summary.
pub(crate) fn infer_finite_list_every_exists(
    operation: &str,
    arguments: &[orna_syntax::Argument],
    input: Option<Inferred>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
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
            Some("predicate") => {
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
    let values = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            if slots[1] == Some(index) && matches!(argument.value, Expr::Lambda { .. }) {
                return Type::Error;
            }
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();
    let rows = input
        .map(|input| input.ty)
        .or_else(|| slots[0].and_then(|index| values.get(index).cloned()))
        .unwrap_or(Type::Error);

    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("finite-list {operation} arguments do not match its static signature"),
        ));
    }
    let Type::List(element) = rows else {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("finite-list {operation} requires a finite list"),
        ));
        return Inferred::error_with(effects);
    };

    let Some(index) = slots[1] else {
        return Inferred::error_with(effects);
    };
    let callback = infer_finite_list_predicate(
        operation,
        &arguments[index].value,
        element.as_ref().clone(),
        scope,
        local,
        diagnostics,
    );
    effects.join(&callback.effects);
    Inferred {
        ty: if !malformed && callback.ty == Type::Bool {
            Type::Bool
        } else {
            Type::Error
        },
        effects,
    }
}

pub(crate) fn infer_relation_call(
    callee: &Expr,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let name = core_relation_call_name(callee)?;
    if !matches!(name, "exists" | "window") {
        return None;
    }
    if !root_collection_intrinsic_is_unshadowed(name, scope, local) {
        return None;
    }
    if name == "window" && !matches!(callee, Expr::Name { .. }) {
        return None;
    }
    if name == "exists" && arguments.len() != 2 {
        return None;
    }
    if name == "window" && arguments.is_empty() {
        diagnostics.push(diag(DIAG_TYPE, "window requires a relation and a size"));
        return Some(Inferred::error());
    }
    let relation = infer(&arguments[0].value, scope, local, diagnostics);
    let Type::Relation(element) = relation.ty else {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("{name} requires a relation as its first argument"),
        ));
        return Some(Inferred::error_with(relation.effects));
    };
    if name == "window" {
        let window = infer_relation_window(&arguments[1..], *element, scope, local, diagnostics);
        let mut effects = relation.effects;
        effects.join(&window.effects);
        return Some(Inferred {
            ty: window.ty,
            effects,
        });
    }
    let callback = infer_relation_callback(
        &arguments[1].value,
        *element,
        Type::Bool,
        scope,
        local,
        diagnostics,
    );
    let mut effects = relation.effects;
    effects.join(&callback.effects);
    Some(Inferred {
        ty: Type::Bool,
        effects,
    })
}

/// Applies the pinned statistics signatures to relation rows and points.
/// Finite-list calls keep using the standard module's generic source signatures.
pub(crate) fn infer_relation_statistics_call(
    callee: &Expr,
    arguments: &[orna_syntax::Argument],
    input: Option<Inferred>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let operation = standard_statistics_module_operation(callee, scope, local)?;
    let (parameters, required) = match operation {
        "mean" | "median" | "variance" | "standard_deviation" => {
            (&["rows", "scale", "rounding"][..], 1)
        }
        "percentile" => (&["rows", "p", "interpolation", "scale", "rounding"][..], 3),
        "sum" | "min" | "max" | "range" | "mode" => (&["rows"][..], 1),
        "histogram" => (&["rows", "bins", "include_final_upper"][..], 2),
        "rate" | "derivative" | "integrate" => (&["points"][..], 1),
        _ => return None,
    };
    let implicit = usize::from(input.is_some());
    let mut malformed = arguments.len() + implicit > parameters.len();
    let mut slots = vec![None; parameters.len()];
    if input.is_some() {
        slots[0] = Some(usize::MAX);
    }
    let mut positional = implicit;
    let mut named_started = false;
    for (index, argument) in arguments.iter().enumerate() {
        let slot = match argument.name.as_deref() {
            Some(name) => {
                named_started = true;
                parameters.iter().position(|parameter| *parameter == name)
            }
            None if named_started => None,
            None => {
                let slot = Some(positional);
                positional += 1;
                slot
            }
        };
        match slot {
            Some(0) if implicit > 0 => malformed = true,
            Some(slot) if slot < slots.len() && slots[slot].is_none() => {
                slots[slot] = Some(index);
            }
            _ => malformed = true,
        }
    }
    if slots[..required].iter().any(Option::is_none) {
        malformed = true;
    }

    if implicit == 0 {
        let index = slots[0]?;
        let mut probe_diagnostics = Vec::new();
        if !matches!(
            infer(
                &arguments[index].value,
                scope,
                local,
                &mut probe_diagnostics
            )
            .ty,
            Type::Relation(_)
        ) {
            return None;
        }
    } else if !matches!(
        input.as_ref().map(|input| &input.ty),
        Some(Type::Relation(_))
    ) {
        return None;
    }

    let mut effects = input
        .as_ref()
        .map_or_else(EffectSummary::default, |input| input.effects.clone());
    let mut values = vec![None; parameters.len()];
    if let Some(input) = input {
        values[0] = Some(input);
    }
    for (index, argument) in arguments.iter().enumerate() {
        let Some(slot) = slots.iter().position(|slot| *slot == Some(index)) else {
            continue;
        };
        let inferred = infer(&argument.value, scope, local, diagnostics);
        effects.join(&inferred.effects);
        values[slot] = Some(inferred);
    }

    let Some(Type::Relation(element)) = values[0].as_ref().map(|value| &value.ty) else {
        return None;
    };
    let element = element.as_ref().clone();
    let mut valid = !malformed;
    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("std.stats.{operation} relation arguments do not match its static signature"),
        ));
    }
    effects.may_fail = true;
    let result = statistics_outcome(
        StatisticsOutcome {
            operation,
            parameters,
            values: &values,
            scope,
        },
        element,
        &mut valid,
        diagnostics,
    );
    Some(Inferred {
        ty: if valid { result } else { Type::Error },
        effects,
    })
}
