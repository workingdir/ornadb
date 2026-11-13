//! Snapshot and checkpoint fold machinery.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use orna_syntax_ast::{Expr, LiteralKind};

use super::*;

/// The scoped form adds the shadowed parameter names and binder contexts the
/// caller already tracks.
pub(crate) fn specialize_dynamic_parameter_snapshot_contexts_scoped(
    ty: &Type,
    call: &SpecializationCall<'_>,
    shadowed_parameters: &BTreeSet<String>,
    binder_contexts: &BTreeMap<String, Type>,
) -> Type {
    let SpecializationCall {
        parameter_names,
        arguments,
        argument_types,
        local,
        call_span,
        historical_context,
    } = *call;
    match ty {
        Type::Applied {
            base,
            arguments: values,
        } if base == "sys.SnapshotRefContext"
            && let [Type::Named(selector)] = values.as_slice()
            && let Some((binder, parameter, captured)) = snapshot_parameter_context(selector)
            && match binder {
                Some(binder) => !shadowed_parameters.contains(&format!("binder:{binder}")),
                None => captured || !shadowed_parameters.contains(parameter),
            } =>
        {
            if let Some(binder) = binder
                && let Some(actual) = binder_contexts.get(binder)
            {
                actual.clone()
            } else {
                let argument_index = parameter_names
                    .and_then(|names| names.iter().position(|name| name == parameter))
                    .and_then(|index| {
                        call_argument_index_for_parameter(parameter_names, index, arguments)
                    });
                if let Some(argument_index) = argument_index
                    && let Some(argument) = arguments.get(argument_index)
                {
                    call_argument_snapshot_context(
                        &argument.value,
                        argument_types.get(argument_index),
                        call_span,
                        local,
                    )
                } else if historical_context.is_some_and(|context| context == ty)
                    || argument_types
                        .iter()
                        .any(|actual| contains_snapshot_context_key(actual, selector))
                    || parameter_names
                        .is_some_and(|names| names.iter().all(|name| name != parameter))
                {
                    // A closure's result may mention its captured selector even
                    // when the current zero-argument call has no parameter to
                    // substitute. Preserve that exact context instead of
                    // inventing a call-site identity. A selector name absent from
                    // this call's parameters belongs to a nested callable and
                    // stays symbolic until that callable is invoked. The reference
                    // requires exact SnapshotRef pinning but leaves this nested
                    // closure specialization detail unspecified. `database.as_of(pin)`
                    // also forwards the context already carried by its argument.
                    ty.clone()
                } else {
                    // An omitted sibling suppresses the entire binding wave.
                    // Keep separate same-named tuple slots separate in the
                    // resulting symbolic pin map by carrying their formal
                    // binder identity alongside the shared call site.
                    // Binderless captured and live selectors can share a
                    // parameter name. Preserve their source identity too, or
                    // a suppressed rebind can collapse distinct map slots.
                    let formal_identity = binder
                        .map(|binder| format!(":formal-binder:{binder}"))
                        .unwrap_or_else(|| format!(":formal-selector:{selector}"));
                    contextual_snapshot_ref(&format!(
                        "selector:dynamic-call:{}:{}..{}:parameter:{parameter}{formal_identity}",
                        call_span.file().unwrap_or("<unknown>"),
                        call_span.start,
                        call_span.end,
                    ))
                }
            }
        }
        Type::Function {
            parameters,
            parameter_names: function_parameter_names,
            default_parameters,
            result,
        } => {
            let mut nested_shadowed_parameters = shadowed_parameters.clone();
            if let Some(names) = function_parameter_names {
                nested_shadowed_parameters.extend(names.iter().cloned());
            }
            for parameter in parameters {
                collect_snapshot_ref_binder_ids(parameter, &mut nested_shadowed_parameters);
            }
            Type::Function {
                parameters: parameters
                    .iter()
                    .map(|parameter| {
                        specialize_dynamic_parameter_snapshot_contexts_scoped(
                            parameter,
                            call,
                            &nested_shadowed_parameters,
                            binder_contexts,
                        )
                    })
                    .collect(),
                parameter_names: function_parameter_names.clone(),
                default_parameters: default_parameters.clone(),
                result: Box::new(specialize_dynamic_parameter_snapshot_contexts_scoped(
                    result,
                    call,
                    &nested_shadowed_parameters,
                    binder_contexts,
                )),
            }
        }
        // The reference leaves checkpoint-map recovery after a failed call
        // unspecified. Rebind each stored selector as its original pin slot
        // so an omitted sibling cannot collapse same-named paired binders.
        Type::Applied {
            base,
            arguments: map_arguments,
        } if base == "semantic.SnapshotContextMap" && is_snapshot_context_map_shape(ty) => {
            let mut merged: Option<Type> = None;
            for selector in map_arguments {
                let Type::Named(selector) = selector else {
                    return ty.clone();
                };
                let context = contextual_snapshot_ref(selector);
                let context = specialize_dynamic_parameter_snapshot_contexts_scoped(
                    &context,
                    call,
                    shadowed_parameters,
                    binder_contexts,
                );
                merged = Some(match merged {
                    Some(previous) => match merge_snapshot_context_map(&previous, &context) {
                        Some(merged) => merged,
                        None => return ty.clone(),
                    },
                    None => context,
                });
            }
            merged.unwrap_or_else(|| ty.clone())
        }
        Type::List(element) => Type::List(Box::new(
            specialize_dynamic_parameter_snapshot_contexts_scoped(
                element,
                call,
                shadowed_parameters,
                binder_contexts,
            ),
        )),
        Type::Range(element) => Type::Range(Box::new(
            specialize_dynamic_parameter_snapshot_contexts_scoped(
                element,
                call,
                shadowed_parameters,
                binder_contexts,
            ),
        )),
        Type::Relation(element) => Type::Relation(Box::new(
            specialize_dynamic_parameter_snapshot_contexts_scoped(
                element,
                call,
                shadowed_parameters,
                binder_contexts,
            ),
        )),
        Type::Stream(element) => Type::Stream(Box::new(
            specialize_dynamic_parameter_snapshot_contexts_scoped(
                element,
                call,
                shadowed_parameters,
                binder_contexts,
            ),
        )),
        Type::Optional(element) => Type::Optional(Box::new(
            specialize_dynamic_parameter_snapshot_contexts_scoped(
                element,
                call,
                shadowed_parameters,
                binder_contexts,
            ),
        )),
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        specialize_dynamic_parameter_snapshot_contexts_scoped(
                            value,
                            call,
                            shadowed_parameters,
                            binder_contexts,
                        ),
                    )
                })
                .collect(),
        ),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| {
                    specialize_dynamic_parameter_snapshot_contexts_scoped(
                        element,
                        call,
                        shadowed_parameters,
                        binder_contexts,
                    )
                })
                .collect(),
        ),
        Type::Applied {
            base,
            arguments: values,
        } => Type::Applied {
            base: base.clone(),
            arguments: values
                .iter()
                .map(|value| {
                    specialize_dynamic_parameter_snapshot_contexts_scoped(
                        value,
                        call,
                        shadowed_parameters,
                        binder_contexts,
                    )
                })
                .collect(),
        },
        _ => ty.clone(),
    }
}

pub(crate) fn call_argument_index_for_position(
    parameter_names: Option<&[String]>,
    parameter_index: usize,
    arguments: &[orna_syntax_ast::Argument],
) -> Option<usize> {
    if parameter_names.is_some() {
        return call_argument_index_for_parameter(parameter_names, parameter_index, arguments);
    }
    arguments
        .iter()
        .enumerate()
        .filter(|(_, argument)| argument.name.is_none())
        .nth(parameter_index)
        .map(|(argument_index, _)| argument_index)
}

/// Pair binder-scoped SnapshotRef leaves in a callable's formal argument
/// shape with the exact contextual values supplied at this call site.
pub(crate) fn collect_snapshot_binder_contexts(
    formal: &Type,
    actual: &Type,
    into: &mut BTreeMap<String, Type>,
) {
    if is_contextual_snapshot_ref(formal) && is_contextual_snapshot_ref(actual) {
        if let Some(binder) = snapshot_ref_context_key(formal).and_then(snapshot_ref_binder_id) {
            into.insert(binder.to_owned(), actual.clone());
        }
        return;
    }
    match (formal, actual) {
        (Type::List(formal), Type::List(actual))
        | (Type::Range(formal), Type::Range(actual))
        | (Type::Relation(formal), Type::Relation(actual))
        | (Type::Stream(formal), Type::Stream(actual))
        | (Type::Optional(formal), Type::Optional(actual)) => {
            collect_snapshot_binder_contexts(formal, actual, into);
        }
        (Type::Record(formal), Type::Record(actual)) => {
            for (name, formal) in formal {
                if let Some(actual) = actual.get(name) {
                    collect_snapshot_binder_contexts(formal, actual, into);
                }
            }
        }
        (Type::Tuple(formal), Type::Tuple(actual)) if formal.len() == actual.len() => {
            for (formal, actual) in formal.iter().zip(actual) {
                collect_snapshot_binder_contexts(formal, actual, into);
            }
        }
        (
            Type::Applied {
                base: formal_base,
                arguments: formal,
            },
            Type::Applied {
                base: actual_base,
                arguments: actual,
            },
        ) if formal_base == actual_base => {
            for (formal, actual) in formal.iter().zip(actual) {
                collect_snapshot_binder_contexts(formal, actual, into);
            }
        }
        _ => {}
    }
}

/// Nested callable parameters shadow matching binder identities through their
/// result type. Walk aggregate patterns as well as direct SnapshotRef values.
pub(crate) fn collect_snapshot_ref_binder_ids(ty: &Type, into: &mut BTreeSet<String>) {
    if is_contextual_snapshot_ref(ty) {
        if let Some((Some(binder), _, _)) =
            snapshot_ref_context_key(ty).and_then(snapshot_parameter_context)
        {
            into.insert(format!("binder:{binder}"));
        }
        return;
    }
    match ty {
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => collect_snapshot_ref_binder_ids(element, into),
        Type::Record(fields) => {
            for value in fields.values() {
                collect_snapshot_ref_binder_ids(value, into);
            }
        }
        Type::Tuple(elements) => {
            for element in elements {
                collect_snapshot_ref_binder_ids(element, into);
            }
        }
        Type::Applied { arguments, .. } => {
            for argument in arguments {
                collect_snapshot_ref_binder_ids(argument, into);
            }
        }
        Type::Quantity { numeric, .. } => collect_snapshot_ref_binder_ids(numeric, into),
        Type::MoneyPerUnit { currency, unit } => {
            collect_snapshot_ref_binder_ids(currency, into);
            collect_snapshot_ref_binder_ids(unit, into);
        }
        _ => {}
    }
}

pub(crate) fn contains_snapshot_context_key(ty: &Type, key: &str) -> bool {
    match ty {
        Type::Applied { base, arguments }
            if base == "sys.SnapshotRefContext"
                && matches!(arguments.as_slice(), [Type::Named(context)] if context == key) =>
        {
            true
        }
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(|parameter| contains_snapshot_context_key(parameter, key))
                || contains_snapshot_context_key(result, key)
        }
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => contains_snapshot_context_key(element, key),
        Type::Record(fields) => fields
            .values()
            .any(|value| contains_snapshot_context_key(value, key)),
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| contains_snapshot_context_key(element, key)),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(|argument| contains_snapshot_context_key(argument, key)),
        _ => false,
    }
}

pub(crate) fn call_argument_index_for_parameter(
    parameter_names: Option<&[String]>,
    parameter_index: usize,
    arguments: &[orna_syntax_ast::Argument],
) -> Option<usize> {
    let names = parameter_names?;
    names.get(parameter_index)?;
    let mut assigned = BTreeSet::new();
    for (argument_index, argument) in arguments.iter().enumerate() {
        let index = if let Some(name) = argument.name.as_deref() {
            names.iter().position(|parameter| parameter == name)
        } else {
            (0..names.len()).find(|index| !assigned.contains(index))
        }?;
        if index == parameter_index {
            return Some(argument_index);
        }
        assigned.insert(index);
    }
    None
}

pub(crate) fn call_argument_snapshot_context(
    argument: &Expr,
    argument_type: Option<&Type>,
    call_span: &SyntaxSpan,
    local: &BTreeMap<String, Symbol>,
) -> Type {
    if let Some(argument_type) = argument_type
        && is_contextual_snapshot_ref(argument_type)
    {
        return argument_type.clone();
    }
    let selector = match argument {
        Expr::Literal {
            text,
            kind: LiteralKind::String,
            ..
        } => serde_json::from_str::<String>(text)
            .unwrap_or_else(|_| text.trim_matches('"').to_owned()),
        Expr::Name { text, .. } if matches!(text.as_str(), "CWD" | "HEAD") => text.clone(),
        Expr::Name { text, .. }
            if local
                .get(text)
                .is_some_and(|symbol| symbol.kind == SymbolKind::Parameter) =>
        {
            format!("parameter:{text}")
        }
        Expr::Group { inner, .. } => {
            return call_argument_snapshot_context(inner, argument_type, call_span, local);
        }
        _ => format!(
            "dynamic-call:{}:{}..{}",
            call_span.file().unwrap_or("<unknown>"),
            call_span.start,
            call_span.end,
        ),
    };
    contextual_snapshot_ref(&format!("selector:{selector}"))
}

/// A typed `SnapshotRef` parameter has no concrete identity while its body is
/// summarized. Parameter references use a caller-specialized key; local aliases
/// use capture keys so same-named nested parameters cannot retarget them, with
/// a lexical binder identity for aliases of shadowing lambda parameters or
/// local SnapshotRef bindings.
/// Uncontextualized non-parameter references stay generic because this semantic
/// pass cannot infer their runtime pin.
pub(crate) fn specialize_snapshot_ref_parameter(
    expression: &Expr,
    snapshot: &Type,
    local: &BTreeMap<String, Symbol>,
) -> Type {
    if is_contextual_snapshot_ref(snapshot) {
        if let Expr::Name { text, .. } = expression
            && local
                .get(text)
                .is_some_and(|symbol| symbol.kind == SymbolKind::Parameter)
            && let Some(selector) = snapshot_ref_context_key(snapshot)
            && let Some(binder) = snapshot_ref_binder_id(selector)
        {
            return contextual_snapshot_ref(&format!(
                "selector:capture:binder:{binder}:parameter:{text}"
            ));
        }
        return snapshot.clone();
    }
    match expression {
        Expr::Name { text, .. }
            if local
                .get(text)
                .is_some_and(|symbol| symbol.kind == SymbolKind::Parameter)
                && snapshot == &Type::Named("sys.SnapshotRef".into()) =>
        {
            // Preserve the captured binding separately from same-named
            // parameters introduced by callbacks returned from this scope.
            contextual_snapshot_ref(&format!("selector:capture:parameter:{text}"))
        }
        Expr::Group { inner, .. } => specialize_snapshot_ref_parameter(inner, snapshot, local),
        _ => snapshot.clone(),
    }
}

pub(crate) fn specialize_historical_database_result(
    result: &Type,
    argument_types: &[Type],
    arguments: &[orna_syntax_ast::Argument],
    local: &BTreeMap<String, Symbol>,
) -> Option<Type> {
    let Type::Applied {
        base,
        arguments: result_arguments,
    } = result
    else {
        return None;
    };
    if base != "sys.DatabaseSnapshot"
        || !matches!(result_arguments.as_slice(), [Type::Named(snapshot)] if snapshot == "sys.SnapshotRef")
    {
        return None;
    }
    let snapshot = argument_types
        .first()
        .cloned()
        .map(|snapshot| {
            let argument = arguments.first().map(|argument| &argument.value);
            argument.map_or(snapshot.clone(), |argument| {
                specialize_snapshot_ref_parameter(argument, &snapshot, local)
            })
        })
        .unwrap_or_else(|| Type::Named("sys.SnapshotRef".into()));
    Some(historical_database_type(snapshot))
}

pub(crate) fn historical_database_type(snapshot: Type) -> Type {
    Type::Applied {
        base: "sys.DatabaseSnapshot".into(),
        arguments: vec![snapshot],
    }
}

pub(crate) fn historical_namespace_type(snapshot: &Type, namespace: &str) -> Type {
    Type::Applied {
        base: "sys.HistoricalNamespace".into(),
        arguments: vec![snapshot.clone(), Type::Named(namespace.into())],
    }
}

pub(crate) fn historical_table_type(snapshot: &Type, table: &str) -> Type {
    Type::Applied {
        base: "sys.HistoricalTable".into(),
        arguments: vec![snapshot.clone(), Type::Named(table.into())],
    }
}

pub(crate) fn historical_callable_type(snapshot: &Type, callable: &Type) -> Type {
    Type::Applied {
        base: "sys.HistoricalCallable".into(),
        arguments: vec![snapshot.clone(), callable.clone()],
    }
}

pub(crate) fn historical_callable_context(ty: &Type) -> Option<&Type> {
    let Type::Applied { base, arguments } = ty else {
        return None;
    };
    (base == "sys.HistoricalCallable")
        .then_some(match arguments.as_slice() {
            [snapshot, Type::Function { .. }] => Some(snapshot),
            _ => None,
        })
        .flatten()
}

pub(crate) fn callable_function_type(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Function { .. } => Some(ty),
        Type::Applied { base, arguments } if base == "sys.HistoricalCallable" => {
            match arguments.as_slice() {
                [_, function @ Type::Function { .. }] => Some(function),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A compatible local checkpoint reset adopts the selected source value's
/// exact identity map. The reference is silent on structured local reset
/// chains; replacement preserves saved selector identities without unioning
/// them with intermediate checkpoints. Error-recovered values are not valid
/// reset sources because their nested pin maps have not been established.
pub(crate) fn pinned_snapshot_reset_type(expected: &Type, selected: &Type) -> Option<Type> {
    (checkpoint_snapshot_maps_are_valid(expected)
        && checkpoint_snapshot_maps_are_valid(selected)
        && !contains_type_error(expected)
        && !contains_type_error(selected)
        && type_contains_pinned_snapshot_identity(expected)
        && type_contains_pinned_snapshot_identity(selected)
        && pinned_snapshot_reset_shape_matches(expected, selected))
    .then(|| selected.clone())
}

/// A saved source may fill a nested Bottom boundary when another checkpoint
/// boundary gives the local a concrete shape. Bottom has no selector identity,
/// so ignore that path while preserving the topology of concrete maps. The
/// selected source supplies the exact complete map for the reset value.
pub(crate) fn pinned_snapshot_reset_shape_matches(expected: &Type, selected: &Type) -> bool {
    if !checkpoint_snapshot_maps_are_valid(expected)
        || !checkpoint_snapshot_maps_are_valid(selected)
    {
        return false;
    }
    let mut maps = Vec::new();
    let mut path = Vec::new();
    if !collect_reset_snapshot_context_maps(expected, selected, &mut maps, &mut path) {
        return false;
    }

    // Keep the established exact-topology rule for ordinary checkpoint resets.
    if snapshot_context_topology_matches(&maps)
        && pinned_snapshot_reset_shape_matches_at_path(expected, selected, false)
    {
        return true;
    }

    // Retain the established sparse-pair reset rule first. The narrow
    // restoration exception handles a multi-field record with both partial
    // and wholly omitted checkpoint pairs.
    if snapshot_context_paired_tuple_identity_shapes_match(&maps, false)
        && pinned_snapshot_sparse_tuple_shape_matches_at_path(expected, selected, false)
    {
        return true;
    }

    record_has_partial_and_empty_checkpoint_pairs(selected)
        && snapshot_context_paired_tuple_identity_shapes_match(&maps, true)
        && pinned_snapshot_sparse_restoration_shape_matches_at_path(expected, selected, false)
}

/// A sparse checkpoint rebind may add or omit singleton selectors while
/// preserving paired tuple identities. Compare the paired tuple sibling lane
/// shapes carried by each selector. Exact resets require the same number of
/// selectors per shape on both sides, so a selector that duplicates an existing
/// shape (aliasing two lanes' omitted identities) is rejected. Selector names are
/// not compared, because a concrete snapshot may differ between a local and its
/// saved source. Omitted sources only require every shape to be present.
/// Record-field sharing has a separate rollback rule and must still pass the
/// exact reset topology check.
pub(crate) fn snapshot_context_paired_tuple_identity_shapes_match(
    pin_maps: &[SnapshotContextMapPair],
    allow_omitted_source_lanes: bool,
) -> bool {
    fn paired_shapes(pin_maps: &[SnapshotContextMapPair], use_expected: bool) -> PairedShapeCounts {
        let mut memberships = BTreeMap::<
            String,
            BTreeMap<Vec<SnapshotTopologyBoundary>, BTreeSet<SnapshotTopologyBoundary>>,
        >::new();
        for pair in pin_maps {
            let selectors = if use_expected {
                &pair.expected
            } else {
                &pair.actual
            };
            let tuple_lanes = pair
                .boundary_path
                .iter()
                .enumerate()
                .filter_map(|(index, boundary)| match boundary {
                    SnapshotTopologyBoundary::TupleElement(_) => {
                        Some((pair.boundary_path[..index].to_vec(), boundary.clone()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for selector in selectors {
                for (parent, lane) in &tuple_lanes {
                    memberships
                        .entry(selector.clone())
                        .or_default()
                        .entry(parent.clone())
                        .or_default()
                        .insert(lane.clone());
                }
            }
        }
        let mut shapes = BTreeMap::new();
        for parent_memberships in memberships.into_values() {
            let paired = parent_memberships
                .into_iter()
                .filter(|(_, lanes)| lanes.len() > 1)
                .collect::<BTreeSet<_>>();
            if !paired.is_empty() {
                *shapes.entry(paired).or_insert(0) += 1;
            }
        }
        shapes
    }

    let expected = paired_shapes(pin_maps, true);
    let selected = paired_shapes(pin_maps, false);
    if allow_omitted_source_lanes {
        // A fully omitted paired checkpoint field can be restored from its
        // sparse saved source after a dense fold, dropping only learned maps.
        selected.keys().all(|shape| expected.contains_key(shape))
    } else {
        !expected.is_empty() && expected == selected
    }
}

/// The reference is silent about map-width restoration for a record that
/// stores several paired checkpoint fields. Support the family-40 shape
/// narrowly: one pair field is partly omitted and a sibling pair is wholly
/// omitted. Closure capture and single-pair rollback behavior stays strict.
pub(crate) fn record_has_partial_and_empty_checkpoint_pairs(ty: &Type) -> bool {
    match ty {
        Type::Record(fields) => {
            let pair_slots = fields
                .values()
                .filter_map(|field| match field {
                    Type::Tuple(slots) if slots.len() > 1 => Some(slots),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let has_empty_pair = pair_slots
                .iter()
                .any(|slots| slots.iter().all(|slot| matches!(slot, Type::Bottom)));
            let has_partial_pair = pair_slots.iter().any(|slots| {
                slots.iter().any(|slot| matches!(slot, Type::Bottom))
                    && slots.iter().any(|slot| !matches!(slot, Type::Bottom))
            });
            (has_empty_pair && has_partial_pair)
                || fields
                    .values()
                    .any(record_has_partial_and_empty_checkpoint_pairs)
        }
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => record_has_partial_and_empty_checkpoint_pairs(inner),
        _ => false,
    }
}

pub(crate) fn pinned_snapshot_reset_shape_matches_at_path(
    expected: &Type,
    selected: &Type,
    input_position: bool,
) -> bool {
    pinned_snapshot_reset_shape_matches_at_path_mode(expected, selected, input_position, false)
}

pub(crate) fn pinned_snapshot_sparse_tuple_shape_matches_at_path(
    expected: &Type,
    selected: &Type,
    input_position: bool,
) -> bool {
    pinned_snapshot_reset_shape_matches_at_path_mode(expected, selected, input_position, true)
}

pub(crate) fn pinned_snapshot_sparse_restoration_shape_matches_at_path(
    expected: &Type,
    selected: &Type,
    input_position: bool,
) -> bool {
    pinned_snapshot_reset_shape_matches_at_path_with_width(
        expected,
        selected,
        input_position,
        true,
        true,
    )
}

pub(crate) fn pinned_snapshot_reset_shape_matches_at_path_mode(
    expected: &Type,
    selected: &Type,
    input_position: bool,
    allow_sparse_tuple_identities: bool,
) -> bool {
    pinned_snapshot_reset_shape_matches_at_path_with_width(
        expected,
        selected,
        input_position,
        allow_sparse_tuple_identities,
        false,
    )
}

pub(crate) fn pinned_snapshot_reset_shape_matches_at_path_with_width(
    expected: &Type,
    selected: &Type,
    input_position: bool,
    allow_sparse_tuple_identities: bool,
    allow_snapshot_context_width_drift: bool,
) -> bool {
    if input_position && matches!(selected, Type::Bottom) && !matches!(expected, Type::Bottom) {
        // A reset cannot narrow an established callable input to Bottom: a
        // previously valid nested snapshot argument would stop being accepted.
        return false;
    }
    if matches!(expected, Type::Bottom) || matches!(selected, Type::Bottom) {
        return true;
    }
    if expected == selected {
        return true;
    }
    if allow_sparse_tuple_identities {
        if is_contextual_snapshot_ref(expected) || is_contextual_snapshot_ref(selected) {
            return is_contextual_snapshot_ref(expected) && is_contextual_snapshot_ref(selected)
                || (allow_snapshot_context_width_drift
                    && is_snapshot_context_map_shape(expected)
                    && is_snapshot_context_map_shape(selected));
        }
        if is_snapshot_context_map_shape(expected) || is_snapshot_context_map_shape(selected) {
            return is_snapshot_context_map_shape(expected)
                && is_snapshot_context_map_shape(selected);
        }
    }
    if !allow_sparse_tuple_identities
        && !type_contains_bottom(expected)
        && !type_contains_bottom(selected)
    {
        return pinned_snapshot_shape_matches(expected, selected);
    }
    match (expected, selected) {
        (Type::List(expected), Type::List(selected))
        | (Type::Range(expected), Type::Range(selected))
        | (Type::Relation(expected), Type::Relation(selected))
        | (Type::Stream(expected), Type::Stream(selected))
        | (Type::Optional(expected), Type::Optional(selected)) => {
            pinned_snapshot_reset_shape_matches_at_path_with_width(
                expected,
                selected,
                input_position,
                allow_sparse_tuple_identities,
                allow_snapshot_context_width_drift,
            )
        }
        (Type::Tuple(expected), Type::Tuple(selected)) if expected.len() == selected.len() => {
            expected.iter().zip(selected).all(|(expected, selected)| {
                pinned_snapshot_reset_shape_matches_at_path_with_width(
                    expected,
                    selected,
                    input_position,
                    allow_sparse_tuple_identities,
                    allow_snapshot_context_width_drift,
                )
            })
        }
        (Type::Record(expected), Type::Record(selected)) if expected.len() == selected.len() => {
            expected.iter().all(|(name, expected)| {
                selected.get(name).is_some_and(|selected| {
                    pinned_snapshot_reset_shape_matches_at_path_with_width(
                        expected,
                        selected,
                        input_position,
                        allow_sparse_tuple_identities,
                        allow_snapshot_context_width_drift,
                    )
                })
            })
        }
        (
            Type::Function {
                parameters: expected_parameters,
                parameter_names: expected_names,
                default_parameters: expected_defaults,
                result: expected_result,
            },
            Type::Function {
                parameters: selected_parameters,
                parameter_names: selected_names,
                default_parameters: selected_defaults,
                result: selected_result,
            },
        ) => {
            expected_names == selected_names
                && expected_defaults == selected_defaults
                && expected_parameters.len() == selected_parameters.len()
                && expected_parameters.iter().zip(selected_parameters).all(
                    |(expected, selected)| {
                        pinned_snapshot_reset_shape_matches_at_path_with_width(
                            expected,
                            selected,
                            !input_position,
                            allow_sparse_tuple_identities,
                            allow_snapshot_context_width_drift,
                        )
                    },
                )
                && pinned_snapshot_reset_shape_matches_at_path_with_width(
                    expected_result,
                    selected_result,
                    input_position,
                    allow_sparse_tuple_identities,
                    allow_snapshot_context_width_drift,
                )
        }
        (
            Type::Applied {
                base: expected_base,
                arguments: expected_arguments,
            },
            Type::Applied {
                base: selected_base,
                arguments: selected_arguments,
            },
        ) if expected_base == selected_base
            && expected_arguments.len() == selected_arguments.len() =>
        {
            expected_arguments
                .iter()
                .zip(selected_arguments)
                .all(|(expected, selected)| {
                    pinned_snapshot_reset_shape_matches_at_path_with_width(
                        expected,
                        selected,
                        input_position,
                        allow_sparse_tuple_identities,
                        allow_snapshot_context_width_drift,
                    )
                })
        }
        (
            Type::MoneyPerUnit {
                currency: expected_currency,
                unit: expected_unit,
            },
            Type::MoneyPerUnit {
                currency: selected_currency,
                unit: selected_unit,
            },
        ) => {
            pinned_snapshot_reset_shape_matches_at_path_with_width(
                expected_currency,
                selected_currency,
                input_position,
                allow_sparse_tuple_identities,
                allow_snapshot_context_width_drift,
            ) && pinned_snapshot_reset_shape_matches_at_path_with_width(
                expected_unit,
                selected_unit,
                input_position,
                allow_sparse_tuple_identities,
                allow_snapshot_context_width_drift,
            )
        }
        _ => expected == selected,
    }
}

pub(crate) fn type_contains_bottom(ty: &Type) -> bool {
    match ty {
        Type::Bottom => true,
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => type_contains_bottom(inner),
        Type::Tuple(elements) => elements.iter().any(type_contains_bottom),
        Type::Record(fields) => fields.values().any(type_contains_bottom),
        Type::Applied { arguments, .. } => arguments.iter().any(type_contains_bottom),
        Type::Function {
            parameters, result, ..
        } => parameters.iter().any(type_contains_bottom) || type_contains_bottom(result),
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_bottom(currency) || type_contains_bottom(unit)
        }
        _ => false,
    }
}

pub(crate) fn collect_reset_snapshot_context_maps(
    expected: &Type,
    selected: &Type,
    maps: &mut Vec<SnapshotContextMapPair>,
    path: &mut Vec<SnapshotTopologyBoundary>,
) -> bool {
    if matches!(expected, Type::Bottom) || matches!(selected, Type::Bottom) {
        return true;
    }
    if !type_contains_bottom(expected) && !type_contains_bottom(selected) {
        return collect_corresponding_snapshot_context_maps_at_path(expected, selected, maps, path);
    }
    match (expected, selected) {
        (Type::List(expected), Type::List(selected)) => collect_reset_maps_at_boundary(
            expected,
            selected,
            maps,
            path,
            SnapshotTopologyBoundary::ListElement,
        ),
        (Type::Range(expected), Type::Range(selected)) => collect_reset_maps_at_boundary(
            expected,
            selected,
            maps,
            path,
            SnapshotTopologyBoundary::RangeElement,
        ),
        (Type::Relation(expected), Type::Relation(selected)) => collect_reset_maps_at_boundary(
            expected,
            selected,
            maps,
            path,
            SnapshotTopologyBoundary::RelationElement,
        ),
        (Type::Stream(expected), Type::Stream(selected)) => collect_reset_maps_at_boundary(
            expected,
            selected,
            maps,
            path,
            SnapshotTopologyBoundary::StreamElement,
        ),
        (Type::Optional(expected), Type::Optional(selected)) => collect_reset_maps_at_boundary(
            expected,
            selected,
            maps,
            path,
            SnapshotTopologyBoundary::OptionalValue,
        ),
        (Type::Tuple(expected), Type::Tuple(selected)) if expected.len() == selected.len() => {
            expected
                .iter()
                .zip(selected)
                .enumerate()
                .all(|(index, (expected, selected))| {
                    collect_reset_maps_at_boundary(
                        expected,
                        selected,
                        maps,
                        path,
                        SnapshotTopologyBoundary::TupleElement(index),
                    )
                })
        }
        (Type::Record(expected), Type::Record(selected)) if expected.len() == selected.len() => {
            expected.iter().all(|(name, expected)| {
                selected.get(name).is_some_and(|selected| {
                    collect_reset_maps_at_boundary(
                        expected,
                        selected,
                        maps,
                        path,
                        SnapshotTopologyBoundary::RecordField(name.clone()),
                    )
                })
            })
        }
        (
            Type::Function {
                parameters: expected_parameters,
                result: expected_result,
                ..
            },
            Type::Function {
                parameters: selected_parameters,
                result: selected_result,
                ..
            },
        ) if expected_parameters.len() == selected_parameters.len() => {
            expected_parameters
                .iter()
                .zip(selected_parameters)
                .enumerate()
                .all(|(index, (expected, selected))| {
                    collect_reset_maps_at_boundary(
                        expected,
                        selected,
                        maps,
                        path,
                        SnapshotTopologyBoundary::FunctionParameter(index),
                    )
                })
                && collect_reset_maps_at_boundary(
                    expected_result,
                    selected_result,
                    maps,
                    path,
                    SnapshotTopologyBoundary::FunctionResult,
                )
        }
        (
            Type::Applied {
                base: expected_base,
                arguments: expected_arguments,
            },
            Type::Applied {
                base: selected_base,
                arguments: selected_arguments,
            },
        ) if expected_base == selected_base
            && expected_arguments.len() == selected_arguments.len() =>
        {
            expected_arguments
                .iter()
                .zip(selected_arguments)
                .enumerate()
                .all(|(index, (expected, selected))| {
                    collect_reset_maps_at_boundary(
                        expected,
                        selected,
                        maps,
                        path,
                        SnapshotTopologyBoundary::AppliedArgument {
                            base: expected_base.clone(),
                            index,
                        },
                    )
                })
        }
        (
            Type::MoneyPerUnit {
                currency: expected_currency,
                unit: expected_unit,
            },
            Type::MoneyPerUnit {
                currency: selected_currency,
                unit: selected_unit,
            },
        ) => {
            collect_reset_maps_at_boundary(
                expected_currency,
                selected_currency,
                maps,
                path,
                SnapshotTopologyBoundary::MoneyCurrency,
            ) && collect_reset_maps_at_boundary(
                expected_unit,
                selected_unit,
                maps,
                path,
                SnapshotTopologyBoundary::MoneyUnit,
            )
        }
        _ => true,
    }
}

pub(crate) fn collect_reset_maps_at_boundary(
    expected: &Type,
    selected: &Type,
    maps: &mut Vec<SnapshotContextMapPair>,
    path: &mut Vec<SnapshotTopologyBoundary>,
    boundary: SnapshotTopologyBoundary,
) -> bool {
    path.push(boundary);
    let matched = collect_reset_snapshot_context_maps(expected, selected, maps, path);
    path.pop();
    matched
}

pub(crate) fn type_contains_pinned_snapshot_identity(ty: &Type) -> bool {
    checkpoint_snapshot_maps_are_valid(ty) && type_contains_valid_pinned_snapshot_identity(ty)
}

/// Paired reset chains can contain symbolic selectors for omitted closure
/// inputs. Keep their lane-specific failure actionable; the reference is
/// silent on diagnostics for incompatible local checkpoint resets.
pub(crate) fn paired_dynamic_snapshot_reset(expected: &Type, selected: &Type) -> bool {
    matches!((expected, selected), (Type::Tuple(expected), Type::Tuple(selected))
        if expected.len() > 1
            && expected.len() == selected.len()
            && expected.iter().any(type_contains_dynamic_snapshot_identity)
            && selected.iter().any(type_contains_dynamic_snapshot_identity))
}

pub(crate) fn type_contains_dynamic_snapshot_identity(ty: &Type) -> bool {
    match ty {
        Type::Applied { base, arguments } if base == "sys.SnapshotRefContext" => {
            matches!(arguments.as_slice(), [Type::Named(selector)] if selector.starts_with("selector:dynamic-call:"))
        }
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => type_contains_dynamic_snapshot_identity(inner),
        Type::Tuple(elements) => elements.iter().any(type_contains_dynamic_snapshot_identity),
        Type::Record(fields) => fields.values().any(type_contains_dynamic_snapshot_identity),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(type_contains_dynamic_snapshot_identity),
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(type_contains_dynamic_snapshot_identity)
                || type_contains_dynamic_snapshot_identity(result)
        }
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_dynamic_snapshot_identity(currency)
                || type_contains_dynamic_snapshot_identity(unit)
        }
        _ => false,
    }
}

pub(crate) fn type_contains_valid_pinned_snapshot_identity(ty: &Type) -> bool {
    match ty {
        Type::Applied { base, .. } if base == "sys.SnapshotRefContext" => {
            is_contextual_snapshot_ref(ty)
        }
        Type::Applied { base, .. } if base == "sys.HistoricalCallable" => {
            historical_callable_context(ty).is_some()
        }
        Type::Applied { base, .. } if base == "semantic.SnapshotContextMap" => {
            is_snapshot_context_map_shape(ty)
        }
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => type_contains_valid_pinned_snapshot_identity(element),
        Type::Record(fields) => fields
            .values()
            .any(type_contains_valid_pinned_snapshot_identity),
        Type::Tuple(elements) => elements
            .iter()
            .any(type_contains_valid_pinned_snapshot_identity),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(type_contains_valid_pinned_snapshot_identity),
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_valid_pinned_snapshot_identity(currency)
                || type_contains_valid_pinned_snapshot_identity(unit)
        }
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(type_contains_valid_pinned_snapshot_identity)
                || type_contains_valid_pinned_snapshot_identity(result)
        }
        _ => false,
    }
}

/// Rebinding a local aggregate may change contextual `SnapshotRef` identities
/// or historical callable pins while preserving its structural shape. The
/// reference fixes identity for historical reads but is silent about structured
/// local rebinding; permit it when the only differences are selector contexts
/// on pinned snapshot values. Saved aliases retain their original identities.
pub(crate) fn pinned_snapshot_shape_matches(expected: &Type, actual: &Type) -> bool {
    if !checkpoint_snapshot_maps_are_valid(expected) || !checkpoint_snapshot_maps_are_valid(actual)
    {
        return false;
    }
    if expected == actual {
        return true;
    }
    if is_snapshot_context_map_shape(expected) && is_snapshot_context_map_shape(actual) {
        return snapshot_context_map_cardinality(expected)
            == snapshot_context_map_cardinality(actual);
    }
    match (expected, actual) {
        (
            Type::Applied {
                base: expected_base,
                arguments: expected_arguments,
            },
            Type::Applied {
                base: actual_base,
                arguments: actual_arguments,
            },
        ) if expected_base == "sys.HistoricalCallable"
            && actual_base == "sys.HistoricalCallable" =>
        {
            match (expected_arguments.as_slice(), actual_arguments.as_slice()) {
                ([expected_snapshot, expected_callable], [actual_snapshot, actual_callable]) => {
                    // The captured context is part of the callable's pin-map
                    // shape: rebinding may replace selectors, but must retain
                    // the same number of captured pins.
                    pinned_snapshot_shape_matches(expected_snapshot, actual_snapshot)
                        && pinned_snapshot_shape_matches(expected_callable, actual_callable)
                }
                _ => false,
            }
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
        ) if expected_base == "sys.SnapshotRefContext"
            && actual_base == "sys.SnapshotRefContext" =>
        {
            matches!(
                (expected_arguments.as_slice(), actual_arguments.as_slice()),
                ([Type::Named(_)], [Type::Named(_)])
            )
        }
        (
            Type::Function {
                parameters: expected_parameters,
                parameter_names: expected_names,
                default_parameters: expected_defaults,
                result: expected_result,
            },
            Type::Function {
                parameters: actual_parameters,
                parameter_names: actual_names,
                default_parameters: actual_defaults,
                result: actual_result,
            },
        ) => {
            expected_names == actual_names
                && expected_defaults == actual_defaults
                && expected_parameters.len() == actual_parameters.len()
                && function_snapshot_pin_identity_topology_matches(
                    expected_parameters,
                    expected_result,
                    actual_parameters,
                    actual_result,
                )
                && expected_parameters
                    .iter()
                    .zip(actual_parameters)
                    .all(|(expected, actual)| pinned_snapshot_shape_matches(expected, actual))
                && pinned_snapshot_shape_matches(expected_result, actual_result)
        }
        (Type::List(expected), Type::List(actual))
        | (Type::Range(expected), Type::Range(actual))
        | (Type::Relation(expected), Type::Relation(actual))
        | (Type::Stream(expected), Type::Stream(actual))
        | (Type::Optional(expected), Type::Optional(actual)) => {
            pinned_snapshot_shape_matches(expected, actual)
        }
        (Type::Record(expected), Type::Record(actual)) => {
            expected.len() == actual.len()
                && record_pin_identity_topology_matches(expected, actual)
                && expected.iter().all(|(name, expected)| {
                    actual
                        .get(name)
                        .is_some_and(|actual| pinned_snapshot_shape_matches(expected, actual))
                })
        }
        (Type::Tuple(expected), Type::Tuple(actual)) => {
            tuple_checkpoint_shape_matches(expected, actual)
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
        ) => {
            expected_base == actual_base
                && expected_arguments.len() == actual_arguments.len()
                && expected_arguments
                    .iter()
                    .zip(actual_arguments)
                    .all(|(expected, actual)| pinned_snapshot_shape_matches(expected, actual))
        }
        (
            Type::MoneyPerUnit {
                currency: expected_currency,
                unit: expected_unit,
            },
            Type::MoneyPerUnit {
                currency: actual_currency,
                unit: actual_unit,
            },
        ) => {
            pinned_snapshot_shape_matches(expected_currency, actual_currency)
                && pinned_snapshot_shape_matches(expected_unit, actual_unit)
        }
        _ => false,
    }
}

/// A function's pinned value boundaries include its inputs and every nested
/// result. Compare them as one topology so rebinding cannot preserve each
/// field's shape while swapping which parameter supplies a nested snapshot.
/// The reference specifies explicit snapshot selection but does not define
/// local function-value rebinds, so preserve the complete boundary relation.
pub(crate) fn function_snapshot_pin_identity_topology_matches(
    expected_parameters: &[Type],
    expected_result: &Type,
    actual_parameters: &[Type],
    actual_result: &Type,
) -> bool {
    if expected_parameters.len() != actual_parameters.len() {
        return false;
    }
    let mut pin_maps = Vec::new();
    let mut path = Vec::new();
    for (index, (expected, actual)) in expected_parameters
        .iter()
        .zip(actual_parameters)
        .enumerate()
    {
        if !collect_corresponding_snapshot_context_maps_at_boundary(
            expected,
            actual,
            &mut pin_maps,
            &mut path,
            SnapshotTopologyBoundary::FunctionParameter(index),
        ) {
            return false;
        }
    }
    let boundaries_match = collect_corresponding_snapshot_context_maps_at_boundary(
        expected_result,
        actual_result,
        &mut pin_maps,
        &mut path,
        SnapshotTopologyBoundary::FunctionResult,
    );

    boundaries_match && snapshot_context_topology_matches(&pin_maps)
}

pub(crate) fn tuple_checkpoint_shape_matches(expected: &[Type], actual: &[Type]) -> bool {
    expected.len() == actual.len()
        && tuple_pin_identity_topology_matches(expected, actual)
        && expected
            .iter()
            .zip(actual)
            .all(|(expected, actual)| pinned_snapshot_shape_matches(expected, actual))
}

/// Preserve the first tuple row's checkpoint-map width during local list
/// promotion. The history reference defines pin identity but not widening a
/// tuple slot when a later row has a different map width, so this edge fails
/// closed instead of silently unioning incompatible slot maps.
pub(crate) fn tuple_checkpoint_promotion_matches(left: &[Type], right: &[Type]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| checkpoint_pin_map_widths_match(left, right))
        && checkpoint_tuple_pin_identity_topology_matches(left, right)
        && tuple_checkpoint_compaction_fold_preserves_pin_identity(left, right)
}

/// Extend a static fold anchor with the first concrete checkpoint subtree at
/// each omitted path. When a tuple has an omitted slot, learn its paired pin
/// topology from the first accepted row as a unit so later parents cannot
/// rebind sharing between its slots. If paired record siblings also contain
/// omissions, leave tuple promotion pathwise so the enclosing sibling rollback
/// remains atomic. The reference does not specify sequential multi-parent tuple
/// folds, so retain the first accepted topology as the conservative rule.
pub(crate) fn checkpoint_topology_anchor_with_first_pins(anchor: &Type, source: &Type) -> Type {
    if type_contains_paired_checkpoint_omissions(anchor) {
        // Promote every complete tuple lane from this parent in one structural
        // pass. An unrelated still-sparse lane must not prevent its complete
        // siblings from teaching the paired topology for later snapshot folds.
        return checkpoint_topology_anchor_with_first_complete_pins(anchor, source);
    }
    checkpoint_topology_anchor_with_first_pins_local(anchor, source)
}

pub(crate) fn checkpoint_topology_anchor_with_first_complete_pins(
    anchor: &Type,
    source: &Type,
) -> Type {
    match (anchor, source) {
        (Type::Record(anchor), Type::Record(source)) if anchor.keys().eq(source.keys()) => {
            Type::Record(
                anchor
                    .iter()
                    .filter_map(|(name, anchor)| {
                        source.get(name).map(|source| {
                            (
                                name.clone(),
                                checkpoint_topology_anchor_with_first_complete_pins(anchor, source),
                            )
                        })
                    })
                    .collect(),
            )
        }
        (Type::Tuple(anchor), Type::Tuple(source)) if anchor.len() == source.len() => {
            let anchor_has_omission =
                type_contains_partially_omitted_pinned_tuple(&Type::Tuple(anchor.clone()))
                    || type_contains_omitted_checkpoint_tuple(&Type::Tuple(anchor.clone()));
            if anchor_has_omission {
                let source_is_complete =
                    !type_contains_partially_omitted_pinned_tuple(&Type::Tuple(source.clone()))
                        && !type_contains_omitted_checkpoint_tuple(&Type::Tuple(source.clone()));
                if source_is_complete
                    && type_contains_pinned_checkpoint_tuple(&Type::Tuple(source.clone()))
                {
                    Type::Tuple(source.clone())
                } else {
                    // Keep an incomplete lane's original anchor. A sibling
                    // tuple can still be promoted independently in this pass.
                    Type::Tuple(anchor.clone())
                }
            } else {
                Type::Tuple(
                    anchor
                        .iter()
                        .zip(source)
                        .map(|(anchor, source)| {
                            checkpoint_topology_anchor_with_first_complete_pins(anchor, source)
                        })
                        .collect(),
                )
            }
        }
        (Type::List(anchor), Type::List(source)) => Type::List(Box::new(
            checkpoint_topology_anchor_with_first_complete_pins(anchor, source),
        )),
        (Type::Range(anchor), Type::Range(source)) => Type::Range(Box::new(
            checkpoint_topology_anchor_with_first_complete_pins(anchor, source),
        )),
        (Type::Relation(anchor), Type::Relation(source)) => Type::Relation(Box::new(
            checkpoint_topology_anchor_with_first_complete_pins(anchor, source),
        )),
        (Type::Stream(anchor), Type::Stream(source)) => Type::Stream(Box::new(
            checkpoint_topology_anchor_with_first_complete_pins(anchor, source),
        )),
        (Type::Optional(anchor), Type::Optional(source)) => Type::Optional(Box::new(
            checkpoint_topology_anchor_with_first_complete_pins(anchor, source),
        )),
        (
            Type::Applied {
                base: anchor_base,
                arguments: anchor_arguments,
            },
            Type::Applied {
                base: source_base,
                arguments: source_arguments,
            },
        ) if anchor_base == source_base && anchor_arguments.len() == source_arguments.len() => {
            Type::Applied {
                base: anchor_base.clone(),
                arguments: anchor_arguments
                    .iter()
                    .zip(source_arguments)
                    .map(|(anchor, source)| {
                        checkpoint_topology_anchor_with_first_complete_pins(anchor, source)
                    })
                    .collect(),
            }
        }
        (
            Type::Function {
                parameters: anchor_parameters,
                parameter_names,
                result: anchor_result,
                default_parameters,
            },
            Type::Function {
                parameters: source_parameters,
                result: source_result,
                ..
            },
        ) if anchor_parameters.len() == source_parameters.len() => Type::Function {
            parameters: anchor_parameters
                .iter()
                .zip(source_parameters)
                .map(|(anchor, source)| {
                    checkpoint_topology_anchor_with_first_complete_pins(anchor, source)
                })
                .collect(),
            parameter_names: parameter_names.clone(),
            result: Box::new(checkpoint_topology_anchor_with_first_complete_pins(
                anchor_result,
                source_result,
            )),
            default_parameters: default_parameters.clone(),
        },
        (
            Type::MoneyPerUnit {
                currency: anchor_currency,
                unit: anchor_unit,
            },
            Type::MoneyPerUnit {
                currency: source_currency,
                unit: source_unit,
            },
        ) => Type::MoneyPerUnit {
            currency: Box::new(checkpoint_topology_anchor_with_first_complete_pins(
                anchor_currency,
                source_currency,
            )),
            unit: Box::new(checkpoint_topology_anchor_with_first_complete_pins(
                anchor_unit,
                source_unit,
            )),
        },
        _ => anchor.clone(),
    }
}

pub(crate) fn checkpoint_topology_anchor_with_first_pins_local(
    anchor: &Type,
    source: &Type,
) -> Type {
    if matches!(anchor, Type::Bottom) {
        return if type_contains_pinned_snapshot_identity(source) {
            source.clone()
        } else {
            anchor.clone()
        };
    }

    match (anchor, source) {
        (Type::Tuple(anchor), Type::Tuple(source)) if anchor.len() == source.len() => {
            // If any paired slot was omitted, filling just that slot loses
            // selector sharing with concrete siblings. Learn the tuple's
            // paired topology from the first accepted row as one unit; later
            // rows may alpha-rename it but cannot split or join the pair.
            if anchor.iter().any(checkpoint_value_is_omitted)
                && source.iter().any(type_contains_pinned_snapshot_identity)
            {
                Type::Tuple(source.clone())
            } else {
                Type::Tuple(
                    anchor
                        .iter()
                        .zip(source)
                        .map(|(anchor, source)| {
                            checkpoint_topology_anchor_with_first_pins_local(anchor, source)
                        })
                        .collect(),
                )
            }
        }
        (Type::Record(anchor), Type::Record(source)) if anchor.keys().eq(source.keys()) => {
            Type::Record(
                anchor
                    .iter()
                    .filter_map(|(name, anchor)| {
                        source.get(name).map(|source| {
                            (
                                name.clone(),
                                checkpoint_topology_anchor_with_first_pins_local(anchor, source),
                            )
                        })
                    })
                    .collect(),
            )
        }
        (Type::List(anchor), Type::List(source)) => Type::List(Box::new(
            checkpoint_topology_anchor_with_first_pins_local(anchor, source),
        )),
        (Type::Range(anchor), Type::Range(source)) => Type::Range(Box::new(
            checkpoint_topology_anchor_with_first_pins_local(anchor, source),
        )),
        (Type::Relation(anchor), Type::Relation(source)) => Type::Relation(Box::new(
            checkpoint_topology_anchor_with_first_pins_local(anchor, source),
        )),
        (Type::Stream(anchor), Type::Stream(source)) => Type::Stream(Box::new(
            checkpoint_topology_anchor_with_first_pins_local(anchor, source),
        )),
        (Type::Optional(anchor), Type::Optional(source)) => Type::Optional(Box::new(
            checkpoint_topology_anchor_with_first_pins_local(anchor, source),
        )),
        (
            Type::Applied {
                base: anchor_base,
                arguments: anchor_arguments,
            },
            Type::Applied {
                base: source_base,
                arguments: source_arguments,
            },
        ) if anchor_base == source_base && anchor_arguments.len() == source_arguments.len() => {
            Type::Applied {
                base: anchor_base.clone(),
                arguments: anchor_arguments
                    .iter()
                    .zip(source_arguments)
                    .map(|(anchor, source)| {
                        checkpoint_topology_anchor_with_first_pins_local(anchor, source)
                    })
                    .collect(),
            }
        }
        (
            Type::Function {
                parameters: anchor_parameters,
                parameter_names,
                result: anchor_result,
                default_parameters,
            },
            Type::Function {
                parameters: source_parameters,
                result: source_result,
                ..
            },
        ) if anchor_parameters.len() == source_parameters.len() => Type::Function {
            parameters: anchor_parameters
                .iter()
                .zip(source_parameters)
                .map(|(anchor, source)| {
                    checkpoint_topology_anchor_with_first_pins_local(anchor, source)
                })
                .collect(),
            parameter_names: parameter_names.clone(),
            result: Box::new(checkpoint_topology_anchor_with_first_pins_local(
                anchor_result,
                source_result,
            )),
            default_parameters: default_parameters.clone(),
        },
        (
            Type::MoneyPerUnit {
                currency: anchor_currency,
                unit: anchor_unit,
            },
            Type::MoneyPerUnit {
                currency: source_currency,
                unit: source_unit,
            },
        ) => Type::MoneyPerUnit {
            currency: Box::new(checkpoint_topology_anchor_with_first_pins_local(
                anchor_currency,
                source_currency,
            )),
            unit: Box::new(checkpoint_topology_anchor_with_first_pins_local(
                anchor_unit,
                source_unit,
            )),
        },
        _ => anchor.clone(),
    }
}

/// Reconcile another parent into a multi-parent tuple checkpoint fold. Source
/// parents keep the same paired selector paths, while singleton labels may be
/// contributed by different sparse lanes. The fold guard rejects any new
/// cross-slot identity introduced by the accumulated union.
pub(crate) fn merge_multi_parent_checkpoint_tuple(
    accumulated: &Type,
    parent: &Type,
    first_parent: &Type,
) -> Option<Type> {
    let (Type::Tuple(accumulated), Type::Tuple(parent), Type::Tuple(first_parent)) =
        (accumulated, parent, first_parent)
    else {
        return None;
    };
    if accumulated.len() != first_parent.len() || parent.len() != first_parent.len() {
        return None;
    }
    let first_parent_value = Type::Tuple(first_parent.clone());
    let parent_value = Type::Tuple(parent.clone());
    if (!checkpoint_value_pin_identity_topology_matches(&first_parent_value, &parent_value)
        && !checkpoint_value_paired_pin_membership_matches(&first_parent_value, &parent_value))
        || !tuple_checkpoint_compaction_fold_preserves_pin_identity(accumulated, parent)
    {
        return None;
    }

    merge_checkpoint_field_map_multi_parent(&Type::Tuple(accumulated.clone()), &parent_value)
}

/// Find a pinned tuple at any structural depth so parent-list inference can
/// make its reconciliation transactional even when records wrap the tuple.
pub(crate) fn type_contains_pinned_checkpoint_tuple(ty: &Type) -> bool {
    match ty {
        Type::Tuple(_) => type_contains_pinned_snapshot_identity(ty),
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => type_contains_pinned_checkpoint_tuple(element),
        Type::Record(fields) => fields.values().any(type_contains_pinned_checkpoint_tuple),
        Type::Applied { arguments, .. } => {
            arguments.iter().any(type_contains_pinned_checkpoint_tuple)
        }
        Type::Function {
            parameters, result, ..
        } => {
            parameters.iter().any(type_contains_pinned_checkpoint_tuple)
                || type_contains_pinned_checkpoint_tuple(result)
        }
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_pinned_checkpoint_tuple(currency)
                || type_contains_pinned_checkpoint_tuple(unit)
        }
        _ => false,
    }
}

/// A tuple whose checkpoint leaves are all omitted still fixes the fold shape.
/// When later rows supply real pins, keep that first-row boundary as the
/// transactional recovery point instead of letting a failed parent leave a
/// partially promoted map behind.
pub(crate) fn type_contains_omitted_checkpoint_tuple(ty: &Type) -> bool {
    match ty {
        Type::Tuple(elements) => {
            checkpoint_value_is_omitted(ty)
                || elements.iter().any(type_contains_omitted_checkpoint_tuple)
        }
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => type_contains_omitted_checkpoint_tuple(element),
        Type::Record(fields) => fields.values().any(type_contains_omitted_checkpoint_tuple),
        Type::Applied { arguments, .. } => {
            arguments.iter().any(type_contains_omitted_checkpoint_tuple)
        }
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(type_contains_omitted_checkpoint_tuple)
                || type_contains_omitted_checkpoint_tuple(result)
        }
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_omitted_checkpoint_tuple(currency)
                || type_contains_omitted_checkpoint_tuple(unit)
        }
        _ => false,
    }
}

/// Find a tuple path that is absent in some parents while carrying snapshot
/// identities in others. Record-field omission is not represented as a tuple
/// of `Bottom` slots, so compare aligned structural paths across the fold.
pub(crate) fn type_sequence_has_omitted_pinned_checkpoint_tuple(types: &[Type]) -> bool {
    let aligned = types.iter().map(Some).collect::<Vec<_>>();
    type_sequence_has_omitted_pinned_checkpoint_tuple_at_path(&aligned)
}

pub(crate) fn type_sequence_has_omitted_pinned_checkpoint_tuple_at_path(
    types: &[Option<&Type>],
) -> bool {
    let contains_pinned_tuple = types
        .iter()
        .flatten()
        .any(|ty| type_contains_pinned_checkpoint_tuple(ty));
    let contains_omission = types
        .iter()
        .any(|ty| ty.is_none_or(checkpoint_value_is_omitted));
    if contains_pinned_tuple && contains_omission {
        return true;
    }
    if types.iter().any(Option::is_none) {
        return false;
    }
    let present = types
        .iter()
        .map(|ty| ty.expect("checked aligned parent type"))
        .collect::<Vec<_>>();
    let Some(first) = present.first() else {
        return false;
    };

    match first {
        Type::Record(_) if present.iter().all(|ty| matches!(ty, Type::Record(_))) => {
            let names = present
                .iter()
                .flat_map(|ty| match ty {
                    Type::Record(fields) => fields.keys().cloned().collect::<Vec<_>>(),
                    _ => Vec::new(),
                })
                .collect::<BTreeSet<_>>();
            names.into_iter().any(|name| {
                let fields = present
                    .iter()
                    .map(|ty| match ty {
                        Type::Record(fields) => fields.get(&name),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                type_sequence_has_omitted_pinned_checkpoint_tuple_at_path(&fields)
            })
        }
        Type::Tuple(_) if present.iter().all(|ty| matches!(ty, Type::Tuple(_))) => {
            let width = present
                .iter()
                .filter_map(|ty| match ty {
                    Type::Tuple(elements) => Some(elements.len()),
                    _ => None,
                })
                .max()
                .unwrap_or_default();
            (0..width).any(|index| {
                let elements = present
                    .iter()
                    .map(|ty| match ty {
                        Type::Tuple(elements) => elements.get(index),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                type_sequence_has_omitted_pinned_checkpoint_tuple_at_path(&elements)
            })
        }
        Type::List(_) if present.iter().all(|ty| matches!(ty, Type::List(_))) => {
            let elements = present
                .iter()
                .map(|ty| match ty {
                    Type::List(element) => Some(element.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            type_sequence_has_omitted_pinned_checkpoint_tuple_at_path(&elements)
        }
        // Relation windows wrap their row collection in a Relation boundary;
        // omissions inside that window still need a checkpoint fold anchor.
        Type::Relation(_) if present.iter().all(|ty| matches!(ty, Type::Relation(_))) => {
            let elements = present
                .iter()
                .map(|ty| match ty {
                    Type::Relation(element) => Some(element.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            type_sequence_has_omitted_pinned_checkpoint_tuple_at_path(&elements)
        }
        Type::Optional(_) if present.iter().all(|ty| matches!(ty, Type::Optional(_))) => {
            let elements = present
                .iter()
                .map(|ty| match ty {
                    Type::Optional(element) => Some(element.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            type_sequence_has_omitted_pinned_checkpoint_tuple_at_path(&elements)
        }
        _ => false,
    }
}

/// Give recursively nested fold rows a neutral shape for pinned tuple paths
/// absent from a sibling. Supplied values still anchor their own fields, while
/// an omitted pinned tuple starts as Bottom at each structural leaf so a
/// concrete row can establish its identity topology. The reference does not
/// define local sparse-record tuple folds; treat omission as an empty
/// contribution and retain the tuple path for later scoped rollback.
pub(crate) fn seed_omitted_pinned_checkpoint_tuple_paths(types: &mut [Type]) {
    fn omitted_shape(value: &Type) -> Type {
        match value {
            Type::Tuple(elements) => Type::Tuple(elements.iter().map(omitted_shape).collect()),
            Type::Record(fields) => Type::Record(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), omitted_shape(value)))
                    .collect(),
            ),
            Type::List(element) => Type::List(Box::new(omitted_shape(element))),
            Type::Relation(element) => Type::Relation(Box::new(omitted_shape(element))),
            Type::Optional(element) => Type::Optional(Box::new(omitted_shape(element))),
            _ => Type::Bottom,
        }
    }

    fn seed_aligned_paths(types: &mut [&mut Type]) {
        let Some(first) = types.first() else {
            return;
        };
        match &**first {
            Type::Record(_) if types.iter().all(|ty| matches!(**ty, Type::Record(_))) => {
                let names = types
                    .iter()
                    .flat_map(|ty| match &**ty {
                        Type::Record(fields) => fields.keys().cloned().collect::<Vec<_>>(),
                        _ => Vec::new(),
                    })
                    .collect::<BTreeSet<_>>();
                for name in names {
                    let template = types.iter().find_map(|ty| {
                        let Type::Record(fields) = &**ty else {
                            return None;
                        };
                        let field = fields.get(&name)?;
                        type_contains_pinned_checkpoint_tuple(field).then(|| field.clone())
                    });
                    if let Some(template) = template
                        && types.iter().any(|ty| match &**ty {
                            Type::Record(fields) => fields
                                .get(&name)
                                .is_none_or(|field| matches!(field, Type::Bottom)),
                            _ => false,
                        })
                    {
                        let omitted = omitted_shape(&template);
                        for ty in types.iter_mut() {
                            let Type::Record(fields) = &mut **ty else {
                                continue;
                            };
                            if fields
                                .get(&name)
                                .is_none_or(|field| matches!(field, Type::Bottom))
                            {
                                fields.insert(name.clone(), omitted.clone());
                            }
                        }
                    }

                    let mut children = types
                        .iter_mut()
                        .filter_map(|ty| match &mut **ty {
                            Type::Record(fields) => fields.get_mut(&name),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    seed_aligned_paths(&mut children);
                }
            }
            Type::Tuple(_) if types.iter().all(|ty| matches!(**ty, Type::Tuple(_))) => {
                let width = types
                    .iter()
                    .filter_map(|ty| match &**ty {
                        Type::Tuple(elements) => Some(elements.len()),
                        _ => None,
                    })
                    .max()
                    .unwrap_or_default();
                for index in 0..width {
                    let template = types.iter().find_map(|ty| {
                        let Type::Tuple(elements) = &**ty else {
                            return None;
                        };
                        let element = elements.get(index)?;
                        type_contains_pinned_checkpoint_tuple(element).then(|| element.clone())
                    });
                    if let Some(template) = template
                        && types.iter().any(|ty| match &**ty {
                            Type::Tuple(elements) => elements
                                .get(index)
                                .is_none_or(|element| matches!(element, Type::Bottom)),
                            _ => false,
                        })
                    {
                        let omitted = omitted_shape(&template);
                        for ty in types.iter_mut() {
                            let Type::Tuple(elements) = &mut **ty else {
                                continue;
                            };
                            if matches!(elements.get(index), Some(Type::Bottom)) {
                                elements[index] = omitted.clone();
                            }
                        }
                    }

                    let mut children = types
                        .iter_mut()
                        .filter_map(|ty| match &mut **ty {
                            Type::Tuple(elements) => elements.get_mut(index),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    seed_aligned_paths(&mut children);
                }
            }
            Type::List(_) if types.iter().all(|ty| matches!(**ty, Type::List(_))) => {
                let mut children = types
                    .iter_mut()
                    .filter_map(|ty| match &mut **ty {
                        Type::List(element) => Some(element.as_mut()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                seed_aligned_paths(&mut children);
            }
            Type::Relation(_) if types.iter().all(|ty| matches!(**ty, Type::Relation(_))) => {
                let mut children = types
                    .iter_mut()
                    .filter_map(|ty| match &mut **ty {
                        Type::Relation(element) => Some(element.as_mut()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                seed_aligned_paths(&mut children);
            }
            Type::Optional(_) if types.iter().all(|ty| matches!(**ty, Type::Optional(_))) => {
                let mut children = types
                    .iter_mut()
                    .filter_map(|ty| match &mut **ty {
                        Type::Optional(element) => Some(element.as_mut()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                seed_aligned_paths(&mut children);
            }
            _ => {}
        }
    }

    let mut aligned = types.iter_mut().collect::<Vec<_>>();
    seed_aligned_paths(&mut aligned);
}

/// Paired records can also anchor a scoped rollback when both contain a
/// partially omitted pinned tuple. A single partial tuple has concrete slots
/// to continue folding, but paired omissions establish a relationship between
/// sibling paths; reject a later cross-path rebind locally so unaffected
/// depths and record siblings can still accumulate labels.
pub(crate) fn type_contains_paired_checkpoint_omissions(ty: &Type) -> bool {
    match ty {
        Type::Record(fields) => {
            let omitted_siblings = fields
                .values()
                .filter(|field| type_contains_partially_omitted_pinned_tuple(field))
                .count();
            omitted_siblings >= 2
                || fields
                    .values()
                    .any(type_contains_paired_checkpoint_omissions)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(type_contains_paired_checkpoint_omissions),
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => type_contains_paired_checkpoint_omissions(element),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(type_contains_paired_checkpoint_omissions),
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(type_contains_paired_checkpoint_omissions)
                || type_contains_paired_checkpoint_omissions(result)
        }
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_paired_checkpoint_omissions(currency)
                || type_contains_paired_checkpoint_omissions(unit)
        }
        _ => false,
    }
}

/// Dense records and tuples can carry a paired tuple fold alongside another
/// pinned identity sibling even when no parent omits a slot. Scope a late
/// rebind failure to the conflicting tuple so the independent sibling keeps
/// the values accumulated through the storm. The reference is silent about
/// this mixed-shape boundary; use the smallest failing tuple as the rollback
/// scope and preserve other computed values.
pub(crate) fn type_contains_paired_pinned_checkpoint_siblings(ty: &Type) -> bool {
    match ty {
        Type::Record(fields) => {
            let tuple_siblings = fields
                .values()
                .filter(|field| type_contains_pinned_checkpoint_tuple(field))
                .count();
            let pinned_siblings = fields
                .values()
                .filter(|field| type_contains_pinned_snapshot_identity(field))
                .count();
            (tuple_siblings > 0 && pinned_siblings > 1)
                || fields
                    .values()
                    .any(type_contains_paired_pinned_checkpoint_siblings)
        }
        Type::Tuple(elements) => {
            let tuple_siblings = elements
                .iter()
                .filter(|element| type_contains_pinned_checkpoint_tuple(element))
                .count();
            let pinned_siblings = elements
                .iter()
                .filter(|element| type_contains_pinned_snapshot_identity(element))
                .count();
            (tuple_siblings > 0 && pinned_siblings > 1)
                || elements
                    .iter()
                    .any(type_contains_paired_pinned_checkpoint_siblings)
        }
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => type_contains_paired_pinned_checkpoint_siblings(element),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(type_contains_paired_pinned_checkpoint_siblings),
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(type_contains_paired_pinned_checkpoint_siblings)
                || type_contains_paired_pinned_checkpoint_siblings(result)
        }
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_paired_pinned_checkpoint_siblings(currency)
                || type_contains_paired_pinned_checkpoint_siblings(unit)
        }
        _ => false,
    }
}

pub(crate) fn type_contains_partially_omitted_pinned_tuple(ty: &Type) -> bool {
    match ty {
        Type::Tuple(elements) => {
            (elements.iter().any(checkpoint_value_is_omitted)
                && elements.iter().any(type_contains_pinned_snapshot_identity))
                || elements
                    .iter()
                    .any(type_contains_partially_omitted_pinned_tuple)
        }
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => type_contains_partially_omitted_pinned_tuple(element),
        Type::Record(fields) => fields
            .values()
            .any(type_contains_partially_omitted_pinned_tuple),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(type_contains_partially_omitted_pinned_tuple),
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(type_contains_partially_omitted_pinned_tuple)
                || type_contains_partially_omitted_pinned_tuple(result)
        }
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_partially_omitted_pinned_tuple(currency)
                || type_contains_partially_omitted_pinned_tuple(unit)
        }
        _ => false,
    }
}

/// A partial tuple still has concrete checkpoint depth labels to fold. Only a
/// tuple whose every slot is omitted anchors the transactional recovery point;
/// nested records and tuples can carry that all-omitted shape at any depth.
pub(crate) fn checkpoint_value_is_omitted(ty: &Type) -> bool {
    match ty {
        Type::Bottom => true,
        Type::Tuple(elements) => {
            !elements.is_empty() && elements.iter().all(checkpoint_value_is_omitted)
        }
        Type::Record(fields) => {
            !fields.is_empty() && fields.values().all(checkpoint_value_is_omitted)
        }
        _ => false,
    }
}

/// Reconcile another parent into a nested checkpoint fold. Source parents
/// retain the first parent's paired selector membership, while singleton
/// labels may come from different sparse tuple lanes. Accumulated maps may
/// grow only without introducing new cross-path identity between siblings.
pub(crate) fn merge_multi_parent_checkpoint_value(
    accumulated: &Type,
    parent: &Type,
    first_parent: &Type,
) -> Option<Type> {
    let paired =
        checkpoint_paired_boundary_fold_topology_matches(accumulated, parent, first_parent);
    let source_topology_matches =
        checkpoint_value_pin_identity_topology_matches(first_parent, parent)
            || checkpoint_value_paired_pin_membership_matches(first_parent, parent);
    let compaction = nested_checkpoint_compaction_fold_preserves_pin_identity(accumulated, parent);
    if !paired || !source_topology_matches || !compaction {
        return None;
    }
    if matches!(first_parent, Type::Tuple(_)) {
        return merge_multi_parent_checkpoint_tuple(accumulated, parent, first_parent);
    }

    merge_checkpoint_field_map_multi_parent(accumulated, parent)
}

/// Reconcile one parent while allowing sparse paired folds to recover locally.
/// A rejected nested scope is restored at its smallest boundary across record,
/// tuple, collection-element, generic applied-argument, and money-dimension
/// paths; unaffected sibling paths keep folding. The caller controls whether
/// that scope stays frozen for later parents or is retried against a learned
/// paired topology. The reference is silent on rollback through these local structures,
/// so use the same smallest-failing-scope rule. Snapshot context maps,
/// historical callable/namespace pairs, and function contracts remain atomic
/// because their identities or binder rules are coupled. Rebuild the effective
/// parent with restored paths before rechecking cross-path pin identity, so
/// local recovery cannot manufacture sharing.
pub(crate) fn merge_multi_parent_checkpoint_value_scoped(
    accumulated: &Type,
    parent: &Type,
    rollback_anchor: &Type,
    topology_anchor: &Type,
    path: &mut Vec<SnapshotTopologyBoundary>,
    rolled_back_paths: &mut BTreeSet<Vec<SnapshotTopologyBoundary>>,
) -> Option<ScopedCheckpointFold> {
    if rolled_back_paths.contains(path) {
        return Some(ScopedCheckpointFold {
            merged: rollback_anchor.clone(),
            effective_parent: rollback_anchor.clone(),
            rejected_scope: false,
        });
    }

    // A parent that omits a pinned tuple path contributes no identity. Keep
    // the accumulated value and topology anchor so a later rebind continues
    // the same fold instead of freezing the tuple at the omission.
    if checkpoint_value_is_omitted(parent) && type_contains_pinned_checkpoint_tuple(topology_anchor)
    {
        return Some(ScopedCheckpointFold {
            merged: accumulated.clone(),
            effective_parent: topology_anchor.clone(),
            rejected_scope: false,
        });
    }

    let has_rolled_back_descendant = rolled_back_paths
        .iter()
        .any(|rolled_back| rolled_back.len() > path.len() && rolled_back.starts_with(path));
    if !has_rolled_back_descendant
        && let Some(merged) =
            merge_multi_parent_checkpoint_value(accumulated, parent, topology_anchor)
    {
        return Some(ScopedCheckpointFold {
            merged,
            effective_parent: parent.clone(),
            rejected_scope: false,
        });
    }

    let Some((mut merged, mut effective_parent, rejected_scope)) = merge_scoped_checkpoint_children(
        accumulated,
        parent,
        rollback_anchor,
        topology_anchor,
        path,
        rolled_back_paths,
    ) else {
        return scoped_checkpoint_rollback(rollback_anchor, path, rolled_back_paths);
    };

    let paired_boundaries_match = checkpoint_paired_boundary_fold_topology_matches(
        accumulated,
        &effective_parent,
        topology_anchor,
    );
    let widths_match = checkpoint_pin_map_widths_match(topology_anchor, &effective_parent);
    let topology_matches =
        checkpoint_value_pin_identity_topology_matches(topology_anchor, &effective_parent);
    let compaction_preserves_identity =
        nested_checkpoint_compaction_fold_preserves_pin_identity(accumulated, &effective_parent);
    if !paired_boundaries_match
        || !widths_match
        || !topology_matches
        || !compaction_preserves_identity
    {
        // A late omission can change widths along with identity topology.
        // Let concrete mismatch paths roll back first, then revalidate widths.
        if rollback_conflicting_checkpoint_paths(
            &mut merged,
            &mut effective_parent,
            rollback_anchor,
            accumulated,
            topology_anchor,
            path,
            rolled_back_paths,
        ) && checkpoint_paired_boundary_fold_topology_matches(
            accumulated,
            &effective_parent,
            topology_anchor,
        ) && checkpoint_pin_map_widths_match(topology_anchor, &effective_parent)
            && checkpoint_value_pin_identity_topology_matches(topology_anchor, &effective_parent)
            && nested_checkpoint_compaction_fold_preserves_pin_identity(
                accumulated,
                &effective_parent,
            )
        {
            return Some(ScopedCheckpointFold {
                merged,
                effective_parent,
                rejected_scope: true,
            });
        }
        return scoped_checkpoint_rollback(rollback_anchor, path, rolled_back_paths);
    }

    Some(ScopedCheckpointFold {
        merged,
        effective_parent,
        rejected_scope,
    })
}

/// Reconciles the two sides of one scoped fold by container kind, returning the
/// merged and effective parent plus whether any nested scope was rejected, or
/// `None` when the caller must roll back.
pub(crate) fn merge_scoped_checkpoint_children(
    accumulated: &Type,
    parent: &Type,
    rollback_anchor: &Type,
    topology_anchor: &Type,
    path: &mut Vec<SnapshotTopologyBoundary>,
    rolled_back_paths: &mut BTreeSet<Vec<SnapshotTopologyBoundary>>,
) -> Option<(Type, Type, bool)> {
    let mut rejected_scope = false;
    let (merged, effective_parent) = match (accumulated, parent, rollback_anchor, topology_anchor) {
        (
            Type::Record(accumulated_fields),
            Type::Record(parent_fields),
            Type::Record(rollback_fields),
            Type::Record(topology_fields),
        ) if accumulated_fields.keys().eq(rollback_fields.keys())
            && accumulated_fields.keys().eq(topology_fields.keys())
            && parent_fields
                .keys()
                .all(|name| accumulated_fields.contains_key(name)) =>
        {
            let mut merged_fields = BTreeMap::new();
            let mut effective_fields = BTreeMap::new();
            for (name, accumulated_field) in accumulated_fields {
                let (Some(rollback_field), Some(topology_field)) =
                    (rollback_fields.get(name), topology_fields.get(name))
                else {
                    return None;
                };
                let Some(parent_field) = parent_fields.get(name) else {
                    if type_contains_pinned_checkpoint_tuple(rollback_field)
                        || type_contains_pinned_checkpoint_tuple(topology_field)
                    {
                        merged_fields.insert(name.clone(), accumulated_field.clone());
                        effective_fields.insert(name.clone(), topology_field.clone());
                        continue;
                    }
                    return None;
                };
                path.push(SnapshotTopologyBoundary::RecordField(name.clone()));
                let field = merge_multi_parent_checkpoint_value_scoped(
                    accumulated_field,
                    parent_field,
                    rollback_field,
                    topology_field,
                    path,
                    rolled_back_paths,
                );
                path.pop();
                let field = field?;
                rejected_scope |= field.rejected_scope;
                merged_fields.insert(name.clone(), field.merged);
                effective_fields.insert(name.clone(), field.effective_parent);
            }
            (Type::Record(merged_fields), Type::Record(effective_fields))
        }
        (
            Type::Tuple(accumulated_slots),
            Type::Tuple(parent_slots),
            Type::Tuple(rollback_slots),
            Type::Tuple(topology_slots),
        ) if accumulated_slots.len() == parent_slots.len()
            && accumulated_slots.len() == rollback_slots.len()
            && accumulated_slots.len() == topology_slots.len() =>
        {
            let mut merged_slots = Vec::with_capacity(accumulated_slots.len());
            let mut effective_slots = Vec::with_capacity(accumulated_slots.len());
            for (index, ((accumulated_slot, parent_slot), (rollback_slot, topology_slot))) in
                accumulated_slots
                    .iter()
                    .zip(parent_slots)
                    .zip(rollback_slots.iter().zip(topology_slots))
                    .enumerate()
            {
                path.push(SnapshotTopologyBoundary::TupleElement(index));
                let slot = merge_multi_parent_checkpoint_value_scoped(
                    accumulated_slot,
                    parent_slot,
                    rollback_slot,
                    topology_slot,
                    path,
                    rolled_back_paths,
                );
                path.pop();
                let slot = slot?;
                rejected_scope |= slot.rejected_scope;
                merged_slots.push(slot.merged);
                effective_slots.push(slot.effective_parent);
            }
            (Type::Tuple(merged_slots), Type::Tuple(effective_slots))
        }
        (
            Type::List(accumulated_element),
            Type::List(parent_element),
            Type::List(rollback_element),
            Type::List(topology_element),
        ) => {
            let child = merge_scoped_checkpoint_child(
                accumulated_element,
                parent_element,
                rollback_element,
                topology_element,
                SnapshotTopologyBoundary::ListElement,
                path,
                rolled_back_paths,
            );
            let child = child?;
            rejected_scope |= child.rejected_scope;
            (
                Type::List(Box::new(child.merged)),
                Type::List(Box::new(child.effective_parent)),
            )
        }
        (
            Type::Range(accumulated_element),
            Type::Range(parent_element),
            Type::Range(rollback_element),
            Type::Range(topology_element),
        ) => {
            let child = merge_scoped_checkpoint_child(
                accumulated_element,
                parent_element,
                rollback_element,
                topology_element,
                SnapshotTopologyBoundary::RangeElement,
                path,
                rolled_back_paths,
            );
            let child = child?;
            rejected_scope |= child.rejected_scope;
            (
                Type::Range(Box::new(child.merged)),
                Type::Range(Box::new(child.effective_parent)),
            )
        }
        (
            Type::Relation(accumulated_element),
            Type::Relation(parent_element),
            Type::Relation(rollback_element),
            Type::Relation(topology_element),
        ) => {
            let child = merge_scoped_checkpoint_child(
                accumulated_element,
                parent_element,
                rollback_element,
                topology_element,
                SnapshotTopologyBoundary::RelationElement,
                path,
                rolled_back_paths,
            );
            let child = child?;
            rejected_scope |= child.rejected_scope;
            (
                Type::Relation(Box::new(child.merged)),
                Type::Relation(Box::new(child.effective_parent)),
            )
        }
        (
            Type::Stream(accumulated_element),
            Type::Stream(parent_element),
            Type::Stream(rollback_element),
            Type::Stream(topology_element),
        ) => {
            let child = merge_scoped_checkpoint_child(
                accumulated_element,
                parent_element,
                rollback_element,
                topology_element,
                SnapshotTopologyBoundary::StreamElement,
                path,
                rolled_back_paths,
            );
            let child = child?;
            rejected_scope |= child.rejected_scope;
            (
                Type::Stream(Box::new(child.merged)),
                Type::Stream(Box::new(child.effective_parent)),
            )
        }
        (
            Type::Optional(accumulated_value),
            Type::Optional(parent_value),
            Type::Optional(rollback_value),
            Type::Optional(topology_value),
        ) => {
            let child = merge_scoped_checkpoint_child(
                accumulated_value,
                parent_value,
                rollback_value,
                topology_value,
                SnapshotTopologyBoundary::OptionalValue,
                path,
                rolled_back_paths,
            );
            let child = child?;
            rejected_scope |= child.rejected_scope;
            (
                Type::Optional(Box::new(child.merged)),
                Type::Optional(Box::new(child.effective_parent)),
            )
        }
        (
            Type::Applied {
                base: accumulated_base,
                arguments: accumulated_arguments,
            },
            Type::Applied {
                base: parent_base,
                arguments: parent_arguments,
            },
            Type::Applied {
                base: rollback_base,
                arguments: rollback_arguments,
            },
            Type::Applied {
                base: topology_base,
                arguments: topology_arguments,
            },
        ) if accumulated_base == parent_base
            && accumulated_base == rollback_base
            && accumulated_base == topology_base
            && accumulated_arguments.len() == parent_arguments.len()
            && accumulated_arguments.len() == rollback_arguments.len()
            && accumulated_arguments.len() == topology_arguments.len()
            && accumulated_base != "sys.HistoricalCallable"
            && accumulated_base != "sys.HistoricalNamespace"
            && !is_snapshot_context_type(accumulated) =>
        {
            let mut merged_arguments = Vec::with_capacity(accumulated_arguments.len());
            let mut effective_arguments = Vec::with_capacity(accumulated_arguments.len());
            for (index, arguments) in accumulated_arguments
                .iter()
                .zip(parent_arguments)
                .zip(rollback_arguments)
                .zip(topology_arguments)
                .enumerate()
            {
                let (
                    ((accumulated_argument, parent_argument), rollback_argument),
                    topology_argument,
                ) = arguments;
                let child = merge_scoped_checkpoint_child(
                    accumulated_argument,
                    parent_argument,
                    rollback_argument,
                    topology_argument,
                    SnapshotTopologyBoundary::AppliedArgument {
                        base: accumulated_base.clone(),
                        index,
                    },
                    path,
                    rolled_back_paths,
                );
                let child = child?;
                rejected_scope |= child.rejected_scope;
                merged_arguments.push(child.merged);
                effective_arguments.push(child.effective_parent);
            }
            (
                Type::Applied {
                    base: accumulated_base.clone(),
                    arguments: merged_arguments,
                },
                Type::Applied {
                    base: accumulated_base.clone(),
                    arguments: effective_arguments,
                },
            )
        }
        (
            Type::MoneyPerUnit {
                currency: accumulated_currency,
                unit: accumulated_unit,
            },
            Type::MoneyPerUnit {
                currency: parent_currency,
                unit: parent_unit,
            },
            Type::MoneyPerUnit {
                currency: rollback_currency,
                unit: rollback_unit,
            },
            Type::MoneyPerUnit {
                currency: topology_currency,
                unit: topology_unit,
            },
        ) => {
            let currency = merge_scoped_checkpoint_child(
                accumulated_currency,
                parent_currency,
                rollback_currency,
                topology_currency,
                SnapshotTopologyBoundary::MoneyCurrency,
                path,
                rolled_back_paths,
            );
            let currency = currency?;
            let unit = merge_scoped_checkpoint_child(
                accumulated_unit,
                parent_unit,
                rollback_unit,
                topology_unit,
                SnapshotTopologyBoundary::MoneyUnit,
                path,
                rolled_back_paths,
            );
            let unit = unit?;
            rejected_scope |= currency.rejected_scope || unit.rejected_scope;
            (
                Type::MoneyPerUnit {
                    currency: Box::new(currency.merged),
                    unit: Box::new(unit.merged),
                },
                Type::MoneyPerUnit {
                    currency: Box::new(currency.effective_parent),
                    unit: Box::new(unit.effective_parent),
                },
            )
        }
        _ => return None,
    };
    Some((merged, effective_parent, rejected_scope))
}

pub(crate) fn merge_scoped_checkpoint_child(
    accumulated: &Type,
    parent: &Type,
    rollback_anchor: &Type,
    topology_anchor: &Type,
    boundary: SnapshotTopologyBoundary,
    path: &mut Vec<SnapshotTopologyBoundary>,
    rolled_back_paths: &mut BTreeSet<Vec<SnapshotTopologyBoundary>>,
) -> Option<ScopedCheckpointFold> {
    path.push(boundary);
    let child = merge_multi_parent_checkpoint_value_scoped(
        accumulated,
        parent,
        rollback_anchor,
        topology_anchor,
        path,
        rolled_back_paths,
    );
    path.pop();
    child
}

pub(crate) fn scoped_checkpoint_rollback(
    rollback_anchor: &Type,
    path: &[SnapshotTopologyBoundary],
    rolled_back_paths: &mut BTreeSet<Vec<SnapshotTopologyBoundary>>,
) -> Option<ScopedCheckpointFold> {
    rolled_back_paths.insert(path.to_vec());
    Some(ScopedCheckpointFold {
        merged: rollback_anchor.clone(),
        effective_parent: rollback_anchor.clone(),
        rejected_scope: true,
    })
}

/// A fold can pass every child independently yet create an identity relation
/// between two sibling paths when their maps are considered together. The
/// reference is silent on this nested rollback boundary, so restore only the
/// concrete pin paths named by that new relation; unrelated depth slots keep
/// accumulating labels in later rows.
pub(crate) fn rollback_conflicting_checkpoint_paths(
    merged: &mut Type,
    effective_parent: &mut Type,
    rollback_anchor: &Type,
    accumulated: &Type,
    topology_anchor: &Type,
    path: &[SnapshotTopologyBoundary],
    rolled_back_paths: &mut BTreeSet<Vec<SnapshotTopologyBoundary>>,
) -> bool {
    let mut implicated_paths = BTreeSet::new();
    if !checkpoint_value_pin_identity_topology_matches(topology_anchor, effective_parent) {
        implicated_paths.extend(checkpoint_topology_mismatch_paths(
            topology_anchor,
            effective_parent,
        ));
    }
    if !nested_checkpoint_compaction_fold_preserves_pin_identity(accumulated, effective_parent) {
        implicated_paths.extend(checkpoint_compaction_mismatch_paths(
            accumulated,
            effective_parent,
        ));
    }

    if implicated_paths.is_empty() {
        return false;
    }
    let scopes = implicated_paths
        .into_iter()
        .filter_map(|scope| {
            let mut full_scope = path.to_vec();
            full_scope.extend(scope);
            let mut rollback_scope = checkpoint_rollback_scope(&full_scope)?;
            if !rollback_scope.starts_with(path) || rollback_scope.len() < path.len() {
                return None;
            }
            let mut relative_scope = rollback_scope[path.len()..].to_vec();
            // An earlier parent can omit the whole callback slot, so a deep
            // result boundary may not exist in the rollback anchor. Fall back
            // only as far as the nearest boundary present on all three sides.
            while checkpoint_value_at_scope(rollback_anchor, &relative_scope).is_none()
                || checkpoint_value_at_scope(merged, &relative_scope).is_none()
                || checkpoint_value_at_scope(effective_parent, &relative_scope).is_none()
            {
                if rollback_scope.len() <= path.len() {
                    return None;
                }
                rollback_scope.pop();
                relative_scope.pop();
            }
            Some((rollback_scope, relative_scope))
        })
        .collect::<BTreeMap<_, _>>();
    let mut rolled_back_any = false;
    for (full_scope, scope) in scopes {
        let (Some(anchor_scope), Some(_), Some(_)) = (
            checkpoint_value_at_scope(rollback_anchor, &scope),
            checkpoint_value_at_scope(merged, &scope),
            checkpoint_value_at_scope(effective_parent, &scope),
        ) else {
            continue;
        };
        let anchor_scope = anchor_scope.clone();
        if rolled_back_paths.contains(&full_scope) {
            continue;
        }
        if checkpoint_replace_value_at_scope(merged, &scope, &anchor_scope)
            && checkpoint_replace_value_at_scope(effective_parent, &scope, &anchor_scope)
        {
            rolled_back_paths.insert(full_scope);
            rolled_back_any = true;
        }
    }
    rolled_back_any
}

pub(crate) fn checkpoint_rollback_scope(
    scope: &[SnapshotTopologyBoundary],
) -> Option<Vec<SnapshotTopologyBoundary>> {
    for (index, boundary) in scope.iter().enumerate() {
        match boundary {
            SnapshotTopologyBoundary::FunctionParameter(_)
            | SnapshotTopologyBoundary::FunctionResult => {
                return None;
            }
            SnapshotTopologyBoundary::AppliedArgument { base, .. }
                if base == "sys.HistoricalCallable" || base == "sys.HistoricalNamespace" =>
            {
                return Some(scope[..index].to_vec());
            }
            _ => {}
        }
    }
    // Preserve the established first structural rollback boundary for direct
    // pin maps and aggregate dimensions. Historical callable pairs carry a
    // coupled captured context, so their sibling tuple slot is the narrowest
    // safe boundary and is returned above.
    scope.first().cloned().map(|boundary| vec![boundary])
}

pub(crate) fn checkpoint_value_at_scope<'a>(
    value: &'a Type,
    scope: &[SnapshotTopologyBoundary],
) -> Option<&'a Type> {
    let Some((boundary, rest)) = scope.split_first() else {
        return Some(value);
    };
    let child = match (value, boundary) {
        (Type::Record(fields), SnapshotTopologyBoundary::RecordField(name)) => fields.get(name)?,
        (Type::Tuple(slots), SnapshotTopologyBoundary::TupleElement(index)) => slots.get(*index)?,
        (Type::List(element), SnapshotTopologyBoundary::ListElement)
        | (Type::Range(element), SnapshotTopologyBoundary::RangeElement)
        | (Type::Relation(element), SnapshotTopologyBoundary::RelationElement)
        | (Type::Stream(element), SnapshotTopologyBoundary::StreamElement)
        | (Type::Optional(element), SnapshotTopologyBoundary::OptionalValue) => element,
        (
            Type::Applied { base, arguments },
            SnapshotTopologyBoundary::AppliedArgument {
                base: boundary_base,
                index,
            },
        ) if base == boundary_base => arguments.get(*index)?,
        (Type::MoneyPerUnit { currency, .. }, SnapshotTopologyBoundary::MoneyCurrency) => currency,
        (Type::MoneyPerUnit { unit, .. }, SnapshotTopologyBoundary::MoneyUnit) => unit,
        _ => return None,
    };
    checkpoint_value_at_scope(child, rest)
}

pub(crate) fn checkpoint_replace_value_at_scope(
    value: &mut Type,
    scope: &[SnapshotTopologyBoundary],
    replacement: &Type,
) -> bool {
    let Some((boundary, rest)) = scope.split_first() else {
        *value = replacement.clone();
        return true;
    };
    let child = match (value, boundary) {
        (Type::Record(fields), SnapshotTopologyBoundary::RecordField(name)) => fields.get_mut(name),
        (Type::Tuple(slots), SnapshotTopologyBoundary::TupleElement(index)) => {
            slots.get_mut(*index)
        }
        (Type::List(element), SnapshotTopologyBoundary::ListElement)
        | (Type::Range(element), SnapshotTopologyBoundary::RangeElement)
        | (Type::Relation(element), SnapshotTopologyBoundary::RelationElement)
        | (Type::Stream(element), SnapshotTopologyBoundary::StreamElement)
        | (Type::Optional(element), SnapshotTopologyBoundary::OptionalValue) => {
            Some(element.as_mut())
        }
        (
            Type::Applied { base, arguments },
            SnapshotTopologyBoundary::AppliedArgument {
                base: boundary_base,
                index,
            },
        ) if base == boundary_base => arguments.get_mut(*index),
        (Type::MoneyPerUnit { currency, .. }, SnapshotTopologyBoundary::MoneyCurrency) => {
            Some(currency.as_mut())
        }
        (Type::MoneyPerUnit { unit, .. }, SnapshotTopologyBoundary::MoneyUnit) => {
            Some(unit.as_mut())
        }
        _ => None,
    };
    child.is_some_and(|child| checkpoint_replace_value_at_scope(child, rest, replacement))
}

pub(crate) fn checkpoint_topology_mismatch_paths(
    expected: &Type,
    actual: &Type,
) -> BTreeSet<Vec<SnapshotTopologyBoundary>> {
    let mut pin_maps = Vec::new();
    if !collect_corresponding_snapshot_context_maps_at_path(
        expected,
        actual,
        &mut pin_maps,
        &mut Vec::new(),
    ) {
        return BTreeSet::new();
    }

    let signatures = |expected_side: bool| {
        let mut labels = BTreeMap::<String, BTreeSet<Vec<SnapshotTopologyBoundary>>>::new();
        for pair in pin_maps
            .iter()
            .filter(|pair| !pair.expected.is_empty() && !pair.actual.is_empty())
        {
            let selectors = if expected_side {
                &pair.expected
            } else {
                &pair.actual
            };
            for selector in selectors {
                labels
                    .entry(selector.clone())
                    .or_default()
                    .insert(pair.boundary_path.clone());
            }
        }
        let mut signatures = BTreeMap::<Vec<Vec<SnapshotTopologyBoundary>>, usize>::new();
        for membership in labels.into_values() {
            *signatures
                .entry(membership.into_iter().collect())
                .or_default() += 1;
        }
        signatures
    };

    let expected = signatures(true);
    let actual = signatures(false);
    expected
        .iter()
        .chain(actual.iter())
        .filter(|(membership, _)| expected.get(*membership) != actual.get(*membership))
        .flat_map(|(membership, _)| membership.iter().cloned())
        .collect()
}

pub(crate) fn checkpoint_compaction_mismatch_paths(
    accumulated: &Type,
    parent: &Type,
) -> BTreeSet<Vec<SnapshotTopologyBoundary>> {
    let mut pin_maps = Vec::new();
    if !collect_corresponding_snapshot_context_maps_at_path(
        accumulated,
        parent,
        &mut pin_maps,
        &mut Vec::new(),
    ) {
        return BTreeSet::new();
    }

    let mut mismatched = BTreeSet::new();
    for (index, pair) in pin_maps.iter().enumerate() {
        let folded = pair
            .expected
            .union(&pair.actual)
            .cloned()
            .collect::<BTreeSet<_>>();
        for other in &pin_maps[index + 1..] {
            let other_folded = other
                .expected
                .union(&other.actual)
                .cloned()
                .collect::<BTreeSet<_>>();
            let shared_before = pair
                .expected
                .intersection(&other.expected)
                .chain(pair.actual.intersection(&other.actual))
                .cloned()
                .collect::<BTreeSet<_>>();
            if folded
                .intersection(&other_folded)
                .cloned()
                .collect::<BTreeSet<_>>()
                != shared_before
            {
                mismatched.insert(pair.boundary_path.clone());
                mismatched.insert(other.boundary_path.clone());
            }
        }
    }
    mismatched
}

/// Compare selector topology for paired record boundaries whose values contain
/// pinned tuples, including boundaries nested through records, tuples, and
/// optional values. Do not cross collection or closure boundaries: their folds
/// have independent rollback scopes. The reference is silent on these nested
/// pairings, so use the direct-record rule recursively. This lets a partially
/// omitted first row acquire its depth relation, then prevents later rows in
/// the same storm from splitting that learned pair.
pub(crate) fn checkpoint_paired_boundary_fold_topology_matches(
    accumulated: &Type,
    parent: &Type,
    topology_anchor: &Type,
) -> bool {
    checkpoint_paired_boundary_value_topology_matches(accumulated, parent, topology_anchor)
}

pub(crate) fn checkpoint_paired_boundary_value_topology_matches(
    accumulated: &Type,
    parent: &Type,
    topology_anchor: &Type,
) -> bool {
    match (accumulated, parent, topology_anchor) {
        (Type::Record(accumulated), Type::Record(parent), Type::Record(anchor))
            if accumulated.keys().eq(parent.keys()) && accumulated.keys().eq(anchor.keys()) =>
        {
            checkpoint_paired_boundary_record_maps_match(accumulated, parent, anchor)
        }
        (Type::Tuple(accumulated), Type::Tuple(parent), Type::Tuple(anchor))
            if accumulated.len() == parent.len() && accumulated.len() == anchor.len() =>
        {
            accumulated
                .iter()
                .zip(parent)
                .zip(anchor)
                .all(|((accumulated, parent), anchor)| {
                    checkpoint_paired_boundary_value_topology_matches(accumulated, parent, anchor)
                })
        }
        (Type::Optional(accumulated), Type::Optional(parent), Type::Optional(anchor)) => {
            checkpoint_paired_boundary_value_topology_matches(accumulated, parent, anchor)
        }
        _ => true,
    }
}

pub(crate) fn checkpoint_paired_boundary_record_maps_match(
    accumulated_fields: &BTreeMap<String, Type>,
    parent_fields: &BTreeMap<String, Type>,
    anchor_fields: &BTreeMap<String, Type>,
) -> bool {
    let paired_field_names = anchor_fields
        .iter()
        .filter_map(|(name, anchor)| {
            (type_contains_pinned_tuple_through_boundaries(anchor)
                && accumulated_fields
                    .get(name)
                    .is_some_and(type_contains_pinned_tuple_through_boundaries)
                && parent_fields
                    .get(name)
                    .is_some_and(type_contains_pinned_tuple_through_boundaries))
            .then_some(name)
        })
        .collect::<Vec<_>>();
    let has_paired_fields = paired_field_names.len() >= 2;

    let mut learned = Vec::new();
    for name in paired_field_names.iter().copied() {
        let (Some(anchor), Some(accumulated), Some(parent)) = (
            anchor_fields.get(name),
            accumulated_fields.get(name),
            parent_fields.get(name),
        ) else {
            continue;
        };
        let mut anchor_to_accumulated = Vec::new();
        let mut anchor_to_parent = Vec::new();
        // A nested shape mismatch is handled by the existing scoped fold. Use
        // only the corresponding maps collected before that boundary.
        let _ = collect_corresponding_snapshot_context_maps_at_path(
            anchor,
            accumulated,
            &mut anchor_to_accumulated,
            &mut Vec::new(),
        );
        let _ = collect_corresponding_snapshot_context_maps_at_path(
            anchor,
            parent,
            &mut anchor_to_parent,
            &mut Vec::new(),
        );
        for pair in &mut anchor_to_accumulated {
            pair.boundary_path
                .insert(0, SnapshotTopologyBoundary::RecordField(name.clone()));
        }
        let parent_maps = anchor_to_parent
            .into_iter()
            .map(|mut pair| {
                pair.boundary_path
                    .insert(0, SnapshotTopologyBoundary::RecordField(name.clone()));
                (pair.boundary_path.clone(), pair)
            })
            .collect::<BTreeMap<_, _>>();
        learned.extend(anchor_to_accumulated.iter().filter_map(|accumulated| {
            let parent = parent_maps.get(&accumulated.boundary_path)?;
            (accumulated.expected.is_empty()
                && !accumulated.actual.is_empty()
                && !parent.actual.is_empty())
            .then(|| SnapshotContextMapPair {
                boundary_path: accumulated.boundary_path.clone(),
                expected: accumulated.actual.clone(),
                actual: parent.actual.clone(),
            })
        }));
    }

    if has_paired_fields {
        let paired_maps = learned
            .iter()
            .filter(|candidate| {
                learned.iter().any(|other| {
                    other.boundary_path != candidate.boundary_path
                        && checkpoint_paths_are_paired_boundaries(
                            &candidate.boundary_path,
                            &other.boundary_path,
                        )
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if paired_maps.len() >= 2 {
            let membership_shapes = |expected: bool| {
                let mut labels = BTreeMap::<String, BTreeSet<Vec<SnapshotTopologyBoundary>>>::new();
                for pair in &paired_maps {
                    let selectors = if expected {
                        &pair.expected
                    } else {
                        &pair.actual
                    };
                    for selector in selectors {
                        labels
                            .entry(selector.clone())
                            .or_default()
                            .insert(pair.boundary_path.clone());
                    }
                }
                labels
                    .into_values()
                    .map(|membership| membership.into_iter().collect::<Vec<_>>())
                    .collect::<BTreeSet<_>>()
            };

            if membership_shapes(true) != membership_shapes(false) {
                return false;
            }
        }
    }

    anchor_fields.iter().all(|(name, anchor)| {
        let (Some(accumulated), Some(parent)) =
            (accumulated_fields.get(name), parent_fields.get(name))
        else {
            return true;
        };
        checkpoint_paired_boundary_value_topology_matches(accumulated, parent, anchor)
    })
}

pub(crate) fn type_contains_pinned_tuple_through_boundaries(ty: &Type) -> bool {
    match ty {
        Type::Record(fields) => fields
            .values()
            .any(type_contains_pinned_tuple_through_boundaries),
        Type::Tuple(elements) => {
            type_contains_pinned_snapshot_identity(ty)
                || elements
                    .iter()
                    .any(type_contains_pinned_tuple_through_boundaries)
        }
        Type::Optional(element) => type_contains_pinned_tuple_through_boundaries(element),
        _ => false,
    }
}

pub(crate) fn checkpoint_paths_are_paired_boundaries(
    left: &[SnapshotTopologyBoundary],
    right: &[SnapshotTopologyBoundary],
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut has_sibling_record_path = false;
    let mut shared_tuple_depth = None;
    for (index, (left, right)) in left.iter().zip(right).enumerate() {
        if left == right {
            if matches!(left, SnapshotTopologyBoundary::TupleElement(_))
                && shared_tuple_depth.is_none()
            {
                shared_tuple_depth = Some(index);
            }
        } else if matches!(
            (left, right),
            (
                SnapshotTopologyBoundary::RecordField(_),
                SnapshotTopologyBoundary::RecordField(_)
            )
        ) {
            if shared_tuple_depth.is_some() {
                return false;
            }
            has_sibling_record_path = true;
        } else {
            return false;
        }
    }
    let Some(shared_tuple_depth_index) = shared_tuple_depth else {
        return false;
    };
    has_sibling_record_path
        && left[..shared_tuple_depth_index].iter().all(|boundary| {
            matches!(
                boundary,
                SnapshotTopologyBoundary::RecordField(_) | SnapshotTopologyBoundary::OptionalValue
            )
        })
}

/// ORNA-CP-003 and ORNA-SYS-136 define explicit snapshot selection, but do not
/// specify multi-parent collection folds. Require each source row to preserve
/// the first row's alpha-equivalent selector membership across structural
/// paths before unioning its maps; this prevents a pin label from changing
/// depth merely because different rows use disjoint selector strings.
pub(crate) fn checkpoint_value_pin_identity_topology_matches(
    expected: &Type,
    actual: &Type,
) -> bool {
    if !checkpoint_pin_map_widths_match(expected, actual) {
        return false;
    }
    let mut pin_maps = Vec::new();
    collect_corresponding_snapshot_context_maps_at_path(
        expected,
        actual,
        &mut pin_maps,
        &mut Vec::new(),
    ) && snapshot_context_topology_matches_over_present_maps(&pin_maps)
}

pub(crate) fn checkpoint_value_paired_pin_membership_matches(
    expected: &Type,
    actual: &Type,
) -> bool {
    fn paired_membership_patterns(
        pin_maps: &[SnapshotContextMapPair],
        expected: bool,
    ) -> PairedShapeCounts {
        let mut memberships = BTreeMap::<
            String,
            BTreeMap<Vec<SnapshotTopologyBoundary>, BTreeSet<SnapshotTopologyBoundary>>,
        >::new();
        for pair in pin_maps {
            let selectors = if expected {
                &pair.expected
            } else {
                &pair.actual
            };
            let sibling_lanes = pair
                .boundary_path
                .iter()
                .enumerate()
                .filter_map(|(index, boundary)| match boundary {
                    SnapshotTopologyBoundary::TupleElement(_)
                    | SnapshotTopologyBoundary::RecordField(_) => {
                        Some((pair.boundary_path[..index].to_vec(), boundary.clone()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for selector in selectors {
                for (parent, lane) in &sibling_lanes {
                    memberships
                        .entry(selector.clone())
                        .or_default()
                        .entry(parent.clone())
                        .or_default()
                        .insert(lane.clone());
                }
            }
        }
        let mut patterns = BTreeMap::new();
        for parent_memberships in memberships.into_values() {
            let paired_membership = parent_memberships
                .into_iter()
                .filter(|(_, siblings)| siblings.len() > 1)
                .collect::<BTreeSet<_>>();
            if !paired_membership.is_empty() {
                *patterns.entry(paired_membership).or_insert(0) += 1;
            }
        }
        patterns
    }

    let mut pin_maps = Vec::new();
    if !collect_corresponding_snapshot_context_maps_at_path(
        expected,
        actual,
        &mut pin_maps,
        &mut Vec::new(),
    ) {
        return false;
    }
    let expected_patterns = paired_membership_patterns(&pin_maps, true);
    let actual_patterns = paired_membership_patterns(&pin_maps, false);
    !expected_patterns.is_empty() && expected_patterns == actual_patterns
}

pub(crate) fn checkpoint_tuple_pin_identity_topology_matches(
    expected: &[Type],
    actual: &[Type],
) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut pin_maps = Vec::new();
    let mut path = Vec::new();
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        if !collect_corresponding_snapshot_context_maps_at_boundary(
            expected,
            actual,
            &mut pin_maps,
            &mut path,
            SnapshotTopologyBoundary::TupleElement(index),
        ) {
            return false;
        }
    }
    snapshot_context_topology_matches_over_present_maps(&pin_maps)
}

/// Ignore a structural path where either source parent omitted a pin. The
/// accumulated-fold guard still prevents later concrete labels from creating
/// cross-path identity; this lets an omitted first row acquire its first real
/// selector topology before subsequent rows are compared against it.
pub(crate) fn snapshot_context_topology_matches_over_present_maps(
    pin_maps: &[SnapshotContextMapPair],
) -> bool {
    let present = pin_maps
        .iter()
        .filter(|pair| !pair.expected.is_empty() && !pair.actual.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    snapshot_context_topology_matches(&present)
}

pub(crate) fn nested_checkpoint_compaction_fold_preserves_pin_identity(
    left: &Type,
    right: &Type,
) -> bool {
    let mut pin_maps = Vec::new();
    if !collect_corresponding_snapshot_context_maps_at_path(
        left,
        right,
        &mut pin_maps,
        &mut Vec::new(),
    ) {
        return false;
    }
    checkpoint_snapshot_fold_preserves_pin_identity(&pin_maps)
}

/// A folded tuple map must not invent cross-slot identity by unioning maps
/// from opposite rows. The reference does not define that compaction case;
/// only promote when every identity shared after the fold was shared within
/// at least one input row.
pub(crate) fn tuple_checkpoint_compaction_fold_preserves_pin_identity(
    left: &[Type],
    right: &[Type],
) -> bool {
    let mut pin_maps = Vec::new();
    let mut path = Vec::new();
    for (index, (left, right)) in left.iter().zip(right).enumerate() {
        if !collect_corresponding_snapshot_context_maps_at_boundary(
            left,
            right,
            &mut pin_maps,
            &mut path,
            SnapshotTopologyBoundary::TupleElement(index),
        ) {
            return false;
        }
    }

    checkpoint_snapshot_fold_preserves_pin_identity(&pin_maps)
}

pub(crate) fn record_checkpoint_promotion_matches(
    left: &BTreeMap<String, Type>,
    right: &BTreeMap<String, Type>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(name, left)| {
            right
                .get(name)
                .is_some_and(|right| checkpoint_pin_map_widths_match(left, right))
        })
        && checkpoint_record_pin_identity_topology_matches(left, right)
        && record_checkpoint_compaction_fold_preserves_pin_identity(left, right)
}

pub(crate) fn checkpoint_record_pin_identity_topology_matches(
    expected: &BTreeMap<String, Type>,
    actual: &BTreeMap<String, Type>,
) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut pin_maps = Vec::new();
    let mut path = Vec::new();
    for (name, expected) in expected {
        let Some(actual) = actual.get(name) else {
            return false;
        };
        if !collect_corresponding_snapshot_context_maps_at_boundary(
            expected,
            actual,
            &mut pin_maps,
            &mut path,
            SnapshotTopologyBoundary::RecordField(name.clone()),
        ) {
            return false;
        }
    }
    snapshot_context_topology_matches_over_present_maps(&pin_maps)
}

pub(crate) fn record_checkpoint_compaction_fold_preserves_pin_identity(
    left: &BTreeMap<String, Type>,
    right: &BTreeMap<String, Type>,
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut pin_maps = Vec::new();
    let mut path = Vec::new();
    for (name, left) in left {
        let Some(right) = right.get(name) else {
            return false;
        };
        if !collect_corresponding_snapshot_context_maps_at_boundary(
            left,
            right,
            &mut pin_maps,
            &mut path,
            SnapshotTopologyBoundary::RecordField(name.clone()),
        ) {
            return false;
        }
    }

    checkpoint_snapshot_fold_preserves_pin_identity(&pin_maps)
}

/// Folding selector maps across collection rows must not invent sharing
/// between structural checkpoint slots that were independent in both rows.
pub(crate) fn checkpoint_snapshot_fold_preserves_pin_identity(
    pin_maps: &[SnapshotContextMapPair],
) -> bool {
    pin_maps.iter().enumerate().all(|(index, pair)| {
        let folded = pair
            .expected
            .union(&pair.actual)
            .cloned()
            .collect::<BTreeSet<_>>();
        pin_maps[index + 1..].iter().all(|other| {
            let folded_other = other
                .expected
                .union(&other.actual)
                .cloned()
                .collect::<BTreeSet<_>>();
            let shared_before = pair
                .expected
                .intersection(&other.expected)
                .chain(pair.actual.intersection(&other.actual))
                .cloned()
                .collect::<BTreeSet<_>>();
            let shared_after = folded
                .intersection(&folded_other)
                .cloned()
                .collect::<BTreeSet<_>>();
            shared_after == shared_before
        })
    })
}

pub(crate) fn checkpoint_pin_map_widths_match(left: &Type, right: &Type) -> bool {
    if matches!(left, Type::Bottom)
        || matches!(right, Type::Bottom)
        || left == &Type::Named("sys.SnapshotRef".into())
        || right == &Type::Named("sys.SnapshotRef".into())
    {
        return true;
    }
    if is_snapshot_context_type(left) || is_snapshot_context_type(right) {
        return is_snapshot_context_map_shape(left)
            && is_snapshot_context_map_shape(right)
            && (snapshot_context_map_cardinality(left) == snapshot_context_map_cardinality(right)
                || snapshot_context_selectors_narrow(left, right));
    }

    match (left, right) {
        (Type::List(left), Type::List(right))
        | (Type::Range(left), Type::Range(right))
        | (Type::Relation(left), Type::Relation(right))
        | (Type::Optional(left), Type::Optional(right)) => {
            checkpoint_pin_map_widths_match(left, right)
        }
        // Stream element identities are scoped to their callback waves and
        // are validated by stream inference; they are not checkpoint tuple
        // data slots for this promotion guard.
        (Type::Stream(_), Type::Stream(_)) => true,
        (Type::Tuple(left), Type::Tuple(right)) if left.len() == right.len() => left
            .iter()
            .zip(right)
            .all(|(left, right)| checkpoint_pin_map_widths_match(left, right)),
        (Type::Record(left), Type::Record(right)) if left.len() == right.len() => {
            left.iter().all(|(name, left)| {
                right
                    .get(name)
                    .is_some_and(|right| checkpoint_pin_map_widths_match(left, right))
            })
        }
        // Function parameter/result compatibility is checked by the function
        // merge arm itself. Tuple promotion only needs to guard data slots;
        // recursively treating callable binders as tuple data breaks
        // transactional recovery for a Bottom call wave.
        (Type::Function { .. }, Type::Function { .. }) => true,
        (
            Type::Applied {
                base: left_base,
                arguments: left_arguments,
            },
            Type::Applied {
                base: right_base,
                arguments: right_arguments,
            },
        ) if left_base == right_base && left_arguments.len() == right_arguments.len() => {
            left_arguments
                .iter()
                .zip(right_arguments)
                .all(|(left, right)| checkpoint_pin_map_widths_match(left, right))
        }
        (
            Type::MoneyPerUnit {
                currency: left_currency,
                unit: left_unit,
            },
            Type::MoneyPerUnit {
                currency: right_currency,
                unit: right_unit,
            },
        ) => {
            checkpoint_pin_map_widths_match(left_currency, right_currency)
                && checkpoint_pin_map_widths_match(left_unit, right_unit)
        }
        _ => true,
    }
}
